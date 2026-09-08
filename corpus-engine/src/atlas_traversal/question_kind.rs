// SPDX-License-Identifier: AGPL-3.0-or-later
//! Question-kind classification — open text onto the closed [`QuestionKind`]
//! set, by centroid (ARCH §2.4, principle 9; `EPISTEMIC_INDEX.md` §2.2).
//!
//! # Why this is not in `classifier.rs`
//!
//! Its sibling [`super::classifier`] answers a different question and keeps
//! doing so. `classify_query` maps a query onto [`super::QueryPlan`] — a
//! TRAVERSAL plan naming a target entity, for `svrn enrich atlas-query`'s
//! brief assembler — and it is a keyword + known-entity-name matcher on
//! purpose: it must run with no embedder, no model and no network, and a miss
//! returns `Unknown` for the caller to fall back on. Nothing here replaces it
//! and no caller of it changes.
//!
//! What this file classifies is the READER'S QUESTION KIND, which selects a
//! row of the navigation table. That is open text over a closed set of five,
//! with no vocabulary to match against — precisely the case §2.4 says is a
//! centroid and not a keyword list.
//!
//! # The method, and where it comes from
//!
//! Nothing new: this is [`crate::extractors::column_aware::HeaderClassifier`]
//! with a different label set — itself a port of the router's
//! `scope_classifier.rs`, which `ARCH_PRINCIPLES` §2.4 names as the pattern.
//! Embed each exemplar, L2-normalise, sum per class, normalise again: that is
//! the centroid. At query time, one dot product per class over a normalised
//! query embedding, then two gates — an absolute similarity floor and a
//! margin over the runner-up. Failing either gate is an ABSTAIN, not a
//! best-guess; the walk then runs [`WalkPolicy::unfiltered`] and says so.
//!
//! # Where the exemplars come from
//!
//! The map, never this file: [`WalkPolicy::exemplars`], through
//! [`NavigationPolicy::classifiable`]. A corpus that phrases a kind its own
//! way retunes the classifier by editing its declaration, and the built-in
//! defaults are the spec's own glosses. This file holds no phrase list at all.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use corpus_engine_vocab::ontology::{NavigationPolicy, QuestionKind};

use crate::extractors::column_aware::l2_normalize;
use crate::types::EmbedFn;
use crate::{Error, Result};

/// Absolute similarity floor. Below it, no kind is close enough to be the
/// question's kind and the walk stays unfiltered.
///
/// 0.34 is [`crate::extractors::column_aware`]'s `HEADER_MIN_SIM`, reused
/// rather than re-derived: same embedding family, same normalised-cosine
/// scale, same "is this near any class at all" question. It is a FLOOR on a
/// five-way race, not a decision threshold — the margin below is the
/// discriminator.
const KIND_MIN_SIM: f32 = 0.34;

/// Margin the winner must hold over the runner-up. A question that sits
/// between two kinds ("what changed about the themes") has not chosen one,
/// and forcing it into a row would walk the wrong edges silently.
const KIND_MIN_MARGIN: f32 = 0.05;

fn env_f32(key: &str, default: f32) -> f32 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(default)
}

/// How a question was assigned its [`QuestionKind`] — carried into the walk
/// ledger and into `ask`'s result text so the row a walk executed is never
/// something the reader has to infer (principle 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindSource {
    /// A centroid won both gates.
    Classified,
    /// The caller named the kind outright (a tool argument, a test).
    Caller,
    /// Every centroid was too far away, or two were too close together.
    Abstained,
    /// The map declared no exemplars on any row, so no centroid exists.
    NoClassifier,
    /// The embedder could not be reached, or its output did not match the
    /// centroid width.
    ClassifierUnavailable,
    /// A centroid won both gates, but its row cannot fire on the atlases in
    /// scope — no seed kind or no edge kind carried
    /// (`atlas::inventory`) — so the walk ran the next admissible row in
    /// race order, or the unfiltered one. Which, and why, rides on
    /// `WalkSelection::inert`.
    RowInert,
}

impl KindSource {
    /// The one-line spelling used in logs, in the walk ledger, and in `ask`'s
    /// degradation lines.
    pub fn as_str(&self) -> &'static str {
        match self {
            KindSource::Classified => "classified",
            KindSource::Caller => "caller-supplied",
            KindSource::Abstained => "abstained",
            KindSource::NoClassifier => "no-classifier",
            KindSource::ClassifierUnavailable => "classifier-unavailable",
            KindSource::RowInert => "row-inert",
        }
    }

    /// Did a centroid actually decide this? Everything else means the walk
    /// ran [`corpus_engine_vocab::ontology::WalkPolicy::unfiltered`] or a
    /// caller's own choice, which is a fact `ask` must state, not hide.
    pub fn is_degradation(&self) -> bool {
        matches!(
            self,
            KindSource::Abstained
                | KindSource::NoClassifier
                | KindSource::ClassifierUnavailable
                | KindSource::RowInert
        )
    }
}

/// One centroid per classifiable [`QuestionKind`], built from the map's own
/// exemplars.
#[derive(Debug)]
pub struct QuestionKindClassifier {
    /// `(kind, unit-length centroid)`, in [`QuestionKind::ALL`] order.
    centroids: Vec<(QuestionKind, Vec<f32>)>,
    min_sim: f32,
    min_margin: f32,
}

/// The winner of the centroid race, gates ignored — for logging and for
/// threshold work.
#[derive(Debug, Clone, Copy)]
pub struct KindScore {
    pub kind: QuestionKind,
    /// Cosine against the winning centroid (both sides unit-length).
    pub sim: f32,
    /// Winner minus runner-up. Zero when only one kind is classifiable.
    pub margin: f32,
}

impl QuestionKindClassifier {
    /// Build the centroids by embedding the map's exemplars.
    ///
    /// Sequential: five kinds × four short phrases is twenty embed calls, and
    /// the embed slot serialises anyway. `None` — not an error — when the map
    /// declares no exemplars on any row; that is the map saying it has no
    /// classifier, which the caller reports as [`KindSource::NoClassifier`]
    /// rather than defaulting to a kind (principle 6).
    pub async fn build(policy: &NavigationPolicy, embed: &EmbedFn) -> Result<Option<Self>> {
        let rows = policy.classifiable();
        if rows.is_empty() {
            tracing::debug!(
                target: "retrieval_audit",
                "question-kind: the map declares no exemplars on any row; no classifier built"
            );
            return Ok(None);
        }
        let mut centroids = Vec::with_capacity(rows.len());
        for (kind, phrases) in rows {
            centroids.push((kind, centroid(phrases, embed).await?));
        }
        tracing::debug!(
            target: "retrieval_audit",
            kinds = centroids.len(),
            dim = centroids[0].1.len(),
            "question-kind: centroids built from the map's exemplars"
        );
        Ok(Some(Self {
            centroids,
            min_sim: env_f32("SOVEREIGN_QUESTION_KIND_MIN_SIM", KIND_MIN_SIM),
            min_margin: env_f32("SOVEREIGN_QUESTION_KIND_MIN_MARGIN", KIND_MIN_MARGIN),
        }))
    }

    /// Build the centroids from vectors directly — the deterministic seam for
    /// tests, mirroring `ClaimClassClassifier::from_centroids`. No embedder,
    /// no network; the vectors are normalised here so a test may pass raw
    /// ones.
    pub fn from_centroids(centroids: Vec<(QuestionKind, Vec<f32>)>) -> Self {
        let centroids = centroids
            .into_iter()
            .map(|(k, mut v)| {
                l2_normalize(&mut v);
                (k, v)
            })
            .collect();
        Self {
            centroids,
            min_sim: env_f32("SOVEREIGN_QUESTION_KIND_MIN_SIM", KIND_MIN_SIM),
            min_margin: env_f32("SOVEREIGN_QUESTION_KIND_MIN_MARGIN", KIND_MIN_MARGIN),
        }
    }

    pub fn with_gates(mut self, min_sim: f32, min_margin: f32) -> Self {
        self.min_sim = min_sim;
        self.min_margin = min_margin;
        self
    }

    /// How many kinds this classifier can reach.
    pub fn kind_count(&self) -> usize {
        self.centroids.len()
    }

    /// The two gates as this classifier holds them: `(min_sim, min_margin)`.
    /// An instrument reporting an abstain names which gate refused, and it
    /// reads the numbers from here rather than re-deriving them (§10.6).
    pub fn gates(&self) -> (f32, f32) {
        (self.min_sim, self.min_margin)
    }

    /// Cosine between every pair of centroids, highest first — how
    /// confusable this map's kinds are with each other under this embedder.
    /// The background a query's `sim` is read against: a winner below the
    /// map's own inter-kind similarity is near nothing in particular.
    pub fn inter_centroid_sims(&self) -> Vec<(QuestionKind, QuestionKind, f32)> {
        let mut out = Vec::new();
        for (i, (ka, ca)) in self.centroids.iter().enumerate() {
            for (kb, cb) in self.centroids.iter().skip(i + 1) {
                out.push((*ka, *kb, dot(ca, cb)));
            }
        }
        out.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        out
    }

    /// Every kind's cosine against the query, best first — the whole race,
    /// not just its winner. `None` for the same reasons as [`Self::best`].
    pub fn race(&self, query_embedding: &[f32]) -> Option<Vec<(QuestionKind, f32)>> {
        let dim = self.centroids.first()?.1.len();
        if query_embedding.len() != dim || dim == 0 {
            return None;
        }
        let mut q = query_embedding.to_vec();
        l2_normalize(&mut q);
        let mut scored: Vec<(QuestionKind, f32)> = self
            .centroids
            .iter()
            .map(|(k, c)| (*k, dot(&q, c)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Some(scored)
    }

    /// Nearest kind with its similarity and margin, gates ignored.
    ///
    /// `None` when the classifier is empty or the query embedding is a
    /// different width from the centroids — a width mismatch is the query
    /// having been embedded in another space, which is unanswerable rather
    /// than a low score (principle 6).
    pub fn best(&self, query_embedding: &[f32]) -> Option<KindScore> {
        let scored = self.race(query_embedding)?;
        let (kind, sim) = scored[0];
        let runner_up = scored.get(1).map(|x| x.1).unwrap_or(0.0);
        Some(KindScore {
            kind,
            sim,
            margin: sim - runner_up,
        })
    }

    /// Classify, or abstain.
    ///
    /// Returns the kind only when the winner clears BOTH gates. An abstain is
    /// a real answer here: the caller runs the unfiltered row and names the
    /// abstain, rather than walking a row the question did not ask for.
    pub fn classify(&self, query_embedding: &[f32]) -> (Option<QuestionKind>, Option<KindScore>) {
        let Some(score) = self.best(query_embedding) else {
            return (None, None);
        };
        let admitted = score.sim >= self.min_sim && score.margin >= self.min_margin;
        (admitted.then_some(score.kind), Some(score))
    }
}

/// Process-wide centroid cache, keyed by the exemplar set the map declared.
///
/// The key is the exemplars themselves, not the corpus id: two corpora that
/// declare the same phrases share one centroid set, and a corpus that retunes
/// its exemplars gets a fresh one without any invalidation call. Identity
/// from essence, not from an address or a counter (principle 8).
///
/// One process embeds in one space, so the space is not part of the key; a
/// query that arrives in a different width is caught by [`Self::best`]'s
/// width check and abstains rather than scoring nonsense.
type Cache = Mutex<HashMap<u64, Option<Arc<QuestionKindClassifier>>>>;
static CACHE: OnceLock<Cache> = OnceLock::new();

/// The shared classifier for a map's exemplars — built once per distinct
/// exemplar set per process.
///
/// `None` means no classifier could be had, and the two reasons are
/// distinguished for the caller by [`KindSource`]: the map declared no
/// exemplars, or the embedder failed. Both are logged here; neither is
/// allowed to look like a low score.
pub async fn shared_classifier(
    policy: &NavigationPolicy,
    embed: &EmbedFn,
) -> Option<Arc<QuestionKindClassifier>> {
    let key = exemplar_key(policy);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(hit) = guard.get(&key) {
            return hit.clone();
        }
    }
    let built = match QuestionKindClassifier::build(policy, embed).await {
        Ok(c) => c.map(Arc::new),
        Err(e) => {
            // NAMED, never silent: a failed embed here means every question
            // this process sees runs the unfiltered row.
            tracing::warn!(
                target: "retrieval_audit",
                error = %e,
                "question-kind: centroids could not be built (embedder unavailable); \
                 every walk runs the unfiltered row until this is fixed"
            );
            None
        }
    };
    if let Ok(mut guard) = cache.lock() {
        guard.insert(key, built.clone());
    }
    built
}

/// A stable digest of the map's exemplar rows — the cache key.
fn exemplar_key(policy: &NavigationPolicy) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (kind, phrases) in policy.rows() {
        kind.as_str().hash(&mut h);
        phrases.exemplars.hash(&mut h);
    }
    h.finish()
}

async fn centroid(phrases: &[String], embed: &EmbedFn) -> Result<Vec<f32>> {
    let mut sum: Option<Vec<f32>> = None;
    for p in phrases {
        let mut e = (embed)(p).await?;
        l2_normalize(&mut e);
        match sum.as_mut() {
            Some(s) if s.len() == e.len() => {
                for (i, v) in e.into_iter().enumerate() {
                    s[i] += v;
                }
            }
            Some(s) => {
                return Err(Error::Extraction(format!(
                    "question-kind centroid: embedding dim mismatch {} vs {}",
                    s.len(),
                    e.len()
                )))
            }
            None => sum = Some(e),
        }
    }
    let mut c =
        sum.ok_or_else(|| Error::Extraction("question-kind centroid: empty exemplar set".into()))?;
    l2_normalize(&mut c);
    Ok(c)
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine_vocab::ontology::WalkPolicy;

    /// Three orthogonal centroids and a query sitting on one of them: the
    /// classifier picks it. Failing input: swap `dot` for a distance.
    #[test]
    fn a_query_on_a_centroid_classifies_to_its_kind() {
        let c = QuestionKindClassifier::from_centroids(vec![
            (QuestionKind::Thematic, vec![1.0, 0.0, 0.0]),
            (QuestionKind::Tension, vec![0.0, 1.0, 0.0]),
            (QuestionKind::Lookup, vec![0.0, 0.0, 1.0]),
        ]);
        let (kind, score) = c.classify(&[0.0, 1.0, 0.0]);
        assert_eq!(kind, Some(QuestionKind::Tension));
        let score = score.unwrap();
        assert!(score.sim > 0.99, "sim {}", score.sim);
        assert!(score.margin > 0.99, "margin {}", score.margin);
    }

    /// A query equidistant between two kinds ABSTAINS rather than picking the
    /// one that sorts first. This is the gate that keeps a between-kinds
    /// question on the unfiltered row instead of walking the wrong edges.
    /// Failing input: drop the `min_margin` term from `classify`.
    #[test]
    fn a_question_between_two_kinds_abstains() {
        let c = QuestionKindClassifier::from_centroids(vec![
            (QuestionKind::Thematic, vec![1.0, 0.0]),
            (QuestionKind::Trajectory, vec![0.0, 1.0]),
        ]);
        let (kind, score) = c.classify(&[1.0, 1.0]);
        assert_eq!(kind, None, "an equidistant query must not be forced");
        let score = score.unwrap();
        assert!(score.margin.abs() < 1e-5, "margin {}", score.margin);
    }

    /// Far from every centroid is also an abstain — the absolute floor, not
    /// just the margin. Failing input: drop the `min_sim` term.
    #[test]
    fn a_query_far_from_every_kind_abstains() {
        let c = QuestionKindClassifier::from_centroids(vec![
            (QuestionKind::Thematic, vec![1.0, 0.0, 0.0]),
            (QuestionKind::Tension, vec![0.0, 1.0, 0.0]),
        ])
        .with_gates(0.34, 0.05);
        // Closer to the first by a wide margin — the margin gate PASSES — so
        // only the similarity floor can reject it. (A first attempt used
        // [0.2, 0.02, 5.0], whose margin is 0.036 and which therefore failed
        // on the wrong gate: it would have passed even with `min_sim`
        // deleted.)
        let (kind, score) = c.classify(&[0.3, 0.0, 1.0]);
        let score = score.unwrap();
        assert!(score.margin > 0.05, "margin {} must clear", score.margin);
        assert!(
            score.sim < 0.34,
            "sim {} must be under the floor",
            score.sim
        );
        assert_eq!(kind, None);
    }

    /// A query embedded in a DIFFERENT space is unanswerable, not
    /// low-scoring. Failing input: let `best` zip mismatched widths.
    #[test]
    fn a_width_mismatch_is_unanswerable_not_a_low_score() {
        let c = QuestionKindClassifier::from_centroids(vec![(
            QuestionKind::Thematic,
            vec![1.0, 0.0, 0.0],
        )]);
        assert!(c.best(&[1.0, 0.0]).is_none());
        let (kind, score) = c.classify(&[1.0, 0.0]);
        assert!(kind.is_none());
        assert!(score.is_none(), "a mismatch has no score to report either");
    }

    /// A map with no exemplars on any row yields NO classifier — not an
    /// empty one that would classify everything to the first kind.
    /// Failing input: return `Some` with zero centroids from `build`.
    #[tokio::test]
    async fn a_map_with_no_exemplars_builds_no_classifier() {
        let mut policy = NavigationPolicy::default();
        for row in [
            &mut policy.thematic,
            &mut policy.trajectory,
            &mut policy.tension,
            &mut policy.enumeration,
            &mut policy.lookup,
        ] {
            row.exemplars.clear();
        }
        assert!(policy.classifiable().is_empty());
        let embed: EmbedFn = std::sync::Arc::new(|_: &str| {
            Box::pin(async { Ok(vec![1.0_f32, 0.0, 0.0]) })
                as std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<f32>>> + Send>>
        });
        assert!(QuestionKindClassifier::build(&policy, &embed)
            .await
            .unwrap()
            .is_none());
    }

    /// The default map is fully classifiable — all five rows carry
    /// exemplars, so no kind is unreachable by construction. Failing input:
    /// drop `exemplars` from any default row.
    #[test]
    fn every_default_row_is_classifiable() {
        let policy = NavigationPolicy::default();
        assert_eq!(policy.classifiable().len(), QuestionKind::ALL.len());
        // …and the unfiltered row is deliberately NOT one of them.
        assert!(WalkPolicy::unfiltered().exemplars.is_empty());
    }

    /// A corpus that switches one row off by emptying its exemplars keeps the
    /// other four, and the switched-off kind can never be classified onto.
    #[test]
    fn an_emptied_row_is_switched_off_not_deleted() {
        let mut policy = NavigationPolicy::default();
        policy.enumeration.exemplars.clear();
        let kinds: Vec<QuestionKind> = policy.classifiable().into_iter().map(|(k, _)| k).collect();
        assert!(!kinds.contains(&QuestionKind::Enumeration));
        assert_eq!(kinds.len(), 4);
        // The ROW is still there to be walked when a caller names the kind.
        assert_eq!(policy.walk(QuestionKind::Enumeration).hops, 0);
    }

    /// Two maps with the same exemplars share a cache key; changing one
    /// phrase changes it. Failing input: key the cache on a corpus id.
    #[test]
    fn the_cache_key_is_the_exemplars_not_an_address() {
        let a = NavigationPolicy::default();
        let b = NavigationPolicy::default();
        assert_eq!(exemplar_key(&a), exemplar_key(&b));
        let mut c = NavigationPolicy::default();
        c.thematic.exemplars.push("what is the gist".into());
        assert_ne!(exemplar_key(&a), exemplar_key(&c));
    }

    /// Every degradation reads as one, and a real classification does not.
    #[test]
    fn only_the_three_failure_sources_are_degradations() {
        assert!(!KindSource::Classified.is_degradation());
        assert!(!KindSource::Caller.is_degradation());
        assert!(KindSource::Abstained.is_degradation());
        assert!(KindSource::NoClassifier.is_degradation());
        assert!(KindSource::ClassifierUnavailable.is_degradation());
    }
}
