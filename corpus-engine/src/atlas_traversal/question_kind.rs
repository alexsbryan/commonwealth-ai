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
//! # Which vector space — the bug this file was born with (fixed 2026-09-08)
//!
//! A question kind is a SPEECH ACT: what the reader is doing (looking one
//! fact up, enumerating, tracing an arc, probing a tension, asking what the
//! whole thing is about). It is not a subject. So the centroids and the query
//! must be embedded under
//! [`sovereign_contracts::embed_quirks::CLASSIFIER_INSTRUCTION`], and
//! [`kind_space_embedding`] is the ONE place that applies it — to the
//! exemplars in [`QuestionKindClassifier::build`] and to the query in
//! [`QuestionKindClassifier::classify_question`], so the two sides cannot
//! drift into different spaces.
//!
//! Until 2026-09-08 both sides were embedded through the retrieval QUERY
//! instruction ("given a search query, retrieve relevant passages that answer
//! the query"), which asks the model to encode TOPIC. Measured on the Conrad
//! bank (`sovereign/bench/chaos_monkey/secret_agent.toml`, 43 questions,
//! `svrn atlas kind --corpus chaos-secret-agent`):
//!
//! | space | top-1 kind correct | classified | of those, correct |
//! |---|---|---|---|
//! | retrieval query (before) | 6/39 | 6/43 | **0** |
//! | unprefixed | 16/39 | 12/43 | 8 |
//! | speech-act (now) | **28/39** | 22/43 | 16 |
//!
//! The symptom was an abstain rate of 37/43 with winners at 0.19-0.43, and it
//! looked like a threshold problem. It was not: every one of the six the old
//! gates *did* admit was the wrong row. The exemplars scored fine against
//! each other (0.50-0.84) because they are contentless phrases and the topic
//! vector of one contentless phrase is near another's — a gate calibrated on
//! its own training set (§18.1).
//!
//! Two other candidates were measured and rejected. Re-centring the centroids
//! on their own mean, in either space, moves top-1 by at most one question
//! (28 → 29) — the collinearity is real but it is not the signal. Centring on
//! the corpus's own `Question` atoms lifts the RETRIEVAL space from 6/39 to
//! 22/39, which confirms the diagnosis, but it is still below the instruction
//! fix, it makes the classifier corpus-dependent (breaking the exemplar-keyed
//! cache, principle 8), and on this corpus all 22 atoms carry one degenerate
//! `question_type`. The instruction is the cause and the whole fix.
//!
//! The router hit the identical wall on its intent axis on 2026-08-04 and
//! reached the same conclusion from an independent bank; `sovereign_core::
//! router_instruction` carries that write-up and the 8-candidate probe that
//! chose the instruction text. Do not re-select it on a proxy.
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
use sovereign_contracts::embed_quirks::classifier_input;

use crate::extractors::column_aware::l2_normalize;
use crate::types::EmbedFn;
use crate::{Error, Result};

/// Absolute similarity floor — a SAME-SPACE guard, not a "how close to a
/// class" threshold.
///
/// It used to be 0.34, borrowed from [`crate::extractors::column_aware`]'s
/// `HEADER_MIN_SIM` on the reasoning that both are normalised cosines from
/// the same embedding family. That reasoning died with the space change: the
/// numbers below are all measured in the speech-act space and none of them
/// are comparable to the old ones.
///
/// **In this space no floor can answer "is this near any class at all", and
/// the measurement says so plainly.** The classifier instruction pulls every
/// input into one narrow cone — the five centroids sit at cosine 0.87-0.92
/// from each other — so on the Conrad bank real questions score 0.765-0.923
/// while `"asdf qwerty zxcv"` scores 0.872, `"ok"` scores 0.907 and
/// `"<html><body><div class=x></div></body></html>"` scores 0.751. Gibberish
/// outscores 30 of the 43 real questions. There is no value that admits
/// questions and rejects noise; the margin does that job, and only the
/// margin.
///
/// What a floor CAN catch here is the failure that produced this whole fix:
/// a query embedded in a different space from the centroids. Scored against
/// speech-act centroids, a retrieval-prefixed query wins at 0.135-0.324 and
/// an unprefixed one at 0.151-0.463, against 0.765-0.965 for anything
/// actually in the space. 0.50 sits in that gap with room on both sides, so
/// [`QuestionKindClassifier::classify`] refuses a cross-space vector loudly
/// instead of ranking it — see the warning it logs.
const KIND_MIN_SIM: f32 = 0.50;

/// Margin the winner must hold over the runner-up. A question that sits
/// between two kinds ("what changed about the themes") has not chosen one,
/// and forcing it into a row would walk the wrong edges silently.
///
/// This is the discriminator (see [`KIND_MIN_SIM`]), and it is set from the
/// map rather than from any evaluation bank: **the gate must admit the map's
/// own glosses.** A classifier that abstains on the phrases its map declares
/// as what a kind sounds like is broken by construction. The five built-in
/// glosses clear 0.039 at worst (`"which characters are there"`), so the gate
/// sits at half that — leaving a corpus whose exemplars are tighter than the
/// defaults room to still classify its own glosses.
///
/// Two independent checks on that value, neither used to pick it. Eighteen
/// non-question probes (`"ok"`, `"banana"`, `"asdf qwerty zxcv"`, a SQL
/// statement, an HTML fragment, a Rust fn) all score margins ≤ 0.022, so 0.02
/// rejects seventeen of eighteen. On the Conrad bank it admits 22 of 43 at
/// 76% precision, against 6 of 43 at 0% before.
const KIND_MIN_MARGIN: f32 = 0.02;

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
    ///
    /// `embed` MUST be the UN-INSTRUCTED embed surface
    /// (`sovereign_core::embed_fn::inference_to_embed_fn`, i.e.
    /// `InferenceProvider::embed`), never the query-side adapter: this
    /// function supplies the classifier instruction itself, through
    /// [`kind_space_embedding`], and a query-side adapter would prefix a
    /// second instruction on top and land the centroids in a fourth space.
    /// The one embed family this workspace ships has an empty
    /// `document_instruction`, which is what makes `embed` un-instructed —
    /// pinned by `router_instruction::embed_is_the_uninstructed_surface`.
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

    /// Classify a query vector, or abstain.
    ///
    /// Returns the kind only when the winner clears BOTH gates. An abstain is
    /// a real answer here: the caller runs the unfiltered row and names the
    /// abstain, rather than walking a row the question did not ask for.
    ///
    /// **The vector must be in the classifier space** — produced by
    /// [`kind_space_embedding`], which is what
    /// [`Self::classify_question`] does for you. A vector embedded any other
    /// way is not merely low-scoring, it is ranking by the wrong quantity;
    /// the [`KIND_MIN_SIM`] floor is positioned to catch exactly that, and
    /// this logs the repair rather than abstaining mutely.
    pub fn classify(&self, query_embedding: &[f32]) -> (Option<QuestionKind>, Option<KindScore>) {
        let Some(score) = self.best(query_embedding) else {
            return (None, None);
        };
        if score.sim < self.min_sim {
            // The only thing that lands a query this far from every centroid
            // in a cone this narrow is a different vector space. Say which
            // repair, not just which gate — a silent abstain here reads as
            // "hard question" and is actually "wrong embedder" (§18.3).
            tracing::warn!(
                target: "retrieval_audit",
                sim = score.sim,
                min_sim = self.min_sim,
                winner = score.kind.as_str(),
                "question-kind: the query vector scores far below the classifier band \
                 (in-space queries win at 0.77-0.97); it was almost certainly embedded in \
                 another space. Route the question TEXT through \
                 `QuestionKindClassifier::classify_question` instead of pre-embedding it."
            );
        }
        let admitted = score.sim >= self.min_sim && score.margin >= self.min_margin;
        (admitted.then_some(score.kind), Some(score))
    }

    /// Classify a question from its TEXT — the entry point that cannot get
    /// the space wrong.
    ///
    /// The centroids were built by embedding the map's exemplars through
    /// [`kind_space_embedding`]; this embeds the query through the same
    /// function with the same `embed`, so the two sides are in one space by
    /// construction rather than by two call sites agreeing (principle 10).
    /// `embed` is the un-instructed surface, as in [`Self::build`].
    pub async fn classify_question(
        &self,
        question: &str,
        embed: &EmbedFn,
    ) -> Result<(Option<QuestionKind>, Option<KindScore>)> {
        let v = kind_space_embedding(question, embed).await?;
        Ok(self.classify(&v))
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

/// Embed `text` in the question-kind classifier space.
///
/// THE seam. Every vector this file ever compares — the map's exemplars in
/// [`centroid`], the query in [`QuestionKindClassifier::classify_question`] —
/// is produced here, so there is exactly one answer to "which space is this
/// classifier in". Two call sites each formatting their own prefix is how the
/// 2026-09-08 defect happened, one prefix apart from how the router's own
/// 2026-08-04 defect happened.
///
/// Not normalised — callers normalise, matching [`centroid`]'s and
/// [`QuestionKindClassifier::race`]'s existing contract.
pub async fn kind_space_embedding(text: &str, embed: &EmbedFn) -> Result<Vec<f32>> {
    (embed)(&classifier_input(text)).await
}

async fn centroid(phrases: &[String], embed: &EmbedFn) -> Result<Vec<f32>> {
    let mut sum: Option<Vec<f32>> = None;
    for p in phrases {
        let mut e = kind_space_embedding(p, embed).await?;
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

    /// THE seam test. Build the centroids and classify a question through the
    /// same embedder, then look at every string the embedder actually saw:
    /// all of them must carry the classifier instruction. This is the one
    /// invariant whose violation produced the defect — exemplars in one
    /// space, query in another, both sides scoring happily.
    ///
    /// Failing input: embed the query as `(embed)(question)` instead of
    /// through `kind_space_embedding`, and the last recorded string is the
    /// bare question. That is exactly the shape the code had before
    /// 2026-09-08, except that there the ASYMMETRY was hidden one level down
    /// in which adapter the caller passed.
    #[tokio::test]
    async fn the_exemplars_and_the_query_are_embedded_in_one_space() {
        use sovereign_contracts::embed_quirks::CLASSIFIER_INSTRUCTION;
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let embed: EmbedFn = Arc::new(move |t: &str| {
            if let Ok(mut g) = sink.lock() {
                g.push(t.to_string());
            }
            Box::pin(async { Ok(vec![1.0_f32, 0.0, 0.0]) })
                as std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<f32>>> + Send>>
        });
        let policy = NavigationPolicy::default();
        let c = QuestionKindClassifier::build(&policy, &embed)
            .await
            .expect("build")
            .expect("the default map is classifiable");
        c.classify_question("who is this character", &embed)
            .await
            .expect("classify");

        let seen = seen.lock().expect("recorder");
        assert!(
            seen.len() > 1,
            "expected exemplar embeds plus the query, got {}",
            seen.len()
        );
        for text in seen.iter() {
            assert!(
                text.starts_with(CLASSIFIER_INSTRUCTION),
                "every side of the comparison must be in the classifier space; \
                 this one was not: {text:?}"
            );
        }
        assert!(
            seen.last()
                .expect("query")
                .ends_with("who is this character"),
            "the query itself must be the last thing embedded"
        );
    }

    /// A query vector from ANOTHER space is refused by the floor rather than
    /// ranked. The measured band: scored against speech-act centroids, a
    /// retrieval-prefixed query wins at 0.135-0.324 and an unprefixed one at
    /// 0.151-0.463, while anything actually in the space wins at 0.765-0.965.
    ///
    /// Failing input: `[0.30, 0.02, 0.0]` — winner cosine 0.998/… ≈ 0.31 once
    /// normalised against the first centroid, i.e. squarely in the
    /// cross-space band, with a wide margin so ONLY the floor can reject it.
    /// Delete the `min_sim` term and this classifies as `Thematic`, which is
    /// what the pre-2026-09-08 code did on every production question.
    #[test]
    fn a_cross_space_query_vector_is_refused_not_ranked() {
        let c = QuestionKindClassifier::from_centroids(vec![
            (QuestionKind::Thematic, vec![1.0, 0.0, 0.0]),
            (QuestionKind::Tension, vec![0.0, 1.0, 0.0]),
        ]);
        // sim = 0.30/sqrt(0.30^2+0.02^2+0.95^2) ≈ 0.30 — the cross-space band.
        let (kind, score) = c.classify(&[0.30, 0.02, 0.95]);
        let score = score.expect("a score is still reported, so the log can name it");
        assert!(
            score.sim < KIND_MIN_SIM,
            "sim {} must land under the same-space floor",
            score.sim
        );
        assert!(
            score.margin >= KIND_MIN_MARGIN,
            "margin {} must clear, so the floor is the only gate under test",
            score.margin
        );
        assert_eq!(kind, None);
    }

    /// The floor is a same-space guard, NOT a "how close to a class" gate,
    /// and this pins why raising it cannot fix a miss.
    ///
    /// The classifier instruction pulls everything into one narrow cone — the
    /// five real centroids sit at cosine 0.87-0.92 from each other — so noise
    /// and real questions interleave on `sim`. Measured: `"asdf qwerty zxcv"`
    /// 0.872 and `"ok"` 0.907, against real bank questions at 0.765-0.923.
    /// Modelled here with two near-collinear centroids: the noise query
    /// OUTSCORES the real one on `sim`, so no floor admits one and rejects the
    /// other, while the margin separates them cleanly.
    ///
    /// Failing input: raise `KIND_MIN_SIM` to reject `noise` and `real` goes
    /// with it — assert below.
    #[test]
    fn the_floor_cannot_separate_noise_from_a_real_question_only_the_margin_can() {
        let a = 0.9_f32.sqrt();
        let b = (1.0_f32 - 0.9).sqrt();
        let c = QuestionKindClassifier::from_centroids(vec![
            (QuestionKind::Thematic, vec![a, b, 0.0]),
            (QuestionKind::Lookup, vec![a, -b, 0.0]),
        ]);
        // Noise: dead on the shared axis — high sim, no margin.
        let noise = c.best(&[1.0, 0.0, 0.0]).expect("scored");
        // A real question: off the shared axis toward one class, and carrying
        // content the centroids do not (the third component) — LOWER sim than
        // the noise, but a real margin. sim 0.867 / margin 0.065.
        let real = c.best(&[0.85, 0.10, 0.45]).expect("scored");

        assert!(
            noise.sim > real.sim,
            "the measured interleaving must hold: noise {} vs real {}",
            noise.sim,
            real.sim
        );
        assert!(
            noise.margin < real.margin,
            "the margin is the discriminator: noise {} vs real {}",
            noise.margin,
            real.margin
        );
        // The consequence, stated as an assertion rather than a comment
        // (§7.2): any floor that rejects the noise also rejects the question.
        for floor in [0.60_f32, 0.80, 0.90, 0.95] {
            assert!(
                !(noise.sim < floor && real.sim >= floor),
                "floor {floor} appeared to separate them; the geometry says it cannot"
            );
        }
        // …and the margin gate does the job the floor cannot.
        assert!(noise.margin < KIND_MIN_MARGIN && real.margin >= KIND_MIN_MARGIN);
    }

    /// The gates are the ones calibrated in the classifier space, not the
    /// retrieval-space pair they replaced. A silent revert to 0.34/0.05 would
    /// leave the floor unable to catch a cross-space vector (0.34 sits inside
    /// the 0.135-0.463 cross-space band) and the margin refusing 17 of the 22
    /// questions this now classifies.
    #[test]
    fn the_gates_are_the_classifier_space_pair() {
        let c =
            QuestionKindClassifier::from_centroids(vec![(QuestionKind::Thematic, vec![1.0, 0.0])]);
        assert_eq!(c.gates(), (KIND_MIN_SIM, KIND_MIN_MARGIN));
        assert_eq!(KIND_MIN_SIM, 0.50);
        assert_eq!(KIND_MIN_MARGIN, 0.02);
        assert!(
            KIND_MIN_SIM > 0.463,
            "the floor must sit above the measured cross-space band"
        );
        assert!(
            KIND_MIN_SIM < 0.765,
            "…and below the lowest in-space question"
        );
        assert!(
            KIND_MIN_MARGIN < 0.039,
            "the gate must admit the tightest built-in gloss"
        );
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
