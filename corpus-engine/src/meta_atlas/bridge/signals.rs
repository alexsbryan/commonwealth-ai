// SPDX-License-Identifier: AGPL-3.0-or-later
//! Graded alignment signals — how strongly are an SEP topic and a
//! Wikipedia topic the *same concept*?
//!
//! Each [`AlignmentSignal`] returns `Option<SignalHit>` (`None` = did
//! not fire) with a graded strength in `0..1`. Signals are **pure**:
//! external facts (Wikipedia link-graph co-neighbour overlap, shared
//! Wikidata QID) are pre-fetched by the orchestrator and handed in via
//! [`SignalContext`], so this module performs no I/O and is fully unit-
//! testable against two in-memory [`BridgeTopic`]s.
//!
//! The [`SignalStack`] folds the per-signal hits into a weighted
//! composite and assigns an [`AlignmentBand`]: `AutoSame` (deterministic
//! confidence — emit a `same` edge with no model call), `Uncertain`
//! (hand to the LLM adjudicator to type), or `Drop`. The thresholds are
//! v1 constants, expected to tune against the `align --dry-run`
//! histogram on the pilot slice.

use std::collections::BTreeSet;

use super::edges::BridgeSignal;
use super::topic_node::BridgeTopic;

/// External facts the orchestrator pre-fetches (so signals stay pure).
#[derive(Debug, Clone, Default)]
pub struct SignalContext {
    /// Fraction of the SEP topic's named entities that appear as
    /// Wikipedia link-graph neighbours of the WP candidate (`0..1`).
    /// Computed by the orchestrator via `WikipediaGraph::co_neighbors`.
    pub co_neighbor_overlap: f32,
    /// The two topics resolve to the same Wikidata QID (near-decisive;
    /// usually unavailable today since SEP carries no QIDs).
    pub shared_wikidata_qid: bool,
    /// Pre-computed SEP→WP embedding cosine (e.g. derived from the ANN
    /// hit's `vector_distance` as `1 - distance`). Lets `EmbeddingSignal`
    /// fire without storing a vector on the Wikipedia topic, so the
    /// orchestrator needn't embed the WP side separately.
    pub embedding_similarity: Option<f32>,
}

/// One signal's contribution to a pair's score.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SignalHit {
    pub signal: BridgeSignal,
    /// Graded strength, `0..1`.
    pub score: f32,
}

/// A graded similarity signal over a candidate `(sep, wiki)` pair.
pub trait AlignmentSignal: Send + Sync {
    fn score(&self, left: &BridgeTopic, right: &BridgeTopic, ctx: &SignalContext)
        -> Option<SignalHit>;
    fn kind(&self) -> BridgeSignal;
}

// ── individual signals ────────────────────────────────────────────

/// Token-Jaccard of the two titles. Exact normalised equality → 1.0.
pub struct NameMatchSignal;
impl AlignmentSignal for NameMatchSignal {
    fn kind(&self) -> BridgeSignal {
        BridgeSignal::NameMatch
    }
    fn score(&self, left: &BridgeTopic, right: &BridgeTopic, _: &SignalContext) -> Option<SignalHit> {
        let j = jaccard(&tokens(&left.title), &tokens(&right.title));
        (j > 0.0).then_some(SignalHit {
            signal: BridgeSignal::NameMatch,
            score: j,
        })
    }
}

/// Cosine of the two concept embeddings. Fires only when both topics
/// carry an embedding (populated at candidate-generation time).
pub struct EmbeddingSignal;
impl AlignmentSignal for EmbeddingSignal {
    fn kind(&self) -> BridgeSignal {
        BridgeSignal::Embedding
    }
    fn score(&self, left: &BridgeTopic, right: &BridgeTopic, ctx: &SignalContext) -> Option<SignalHit> {
        // Prefer a direct cosine of stored vectors; otherwise fall back
        // to the orchestrator's pre-computed similarity (the ANN hit's
        // `1 - vector_distance`).
        let c = match (left.embedding.as_deref(), right.embedding.as_deref()) {
            (Some(a), Some(b)) => cosine(a, b)?.max(0.0),
            _ => ctx.embedding_similarity?.max(0.0),
        };
        Some(SignalHit {
            signal: BridgeSignal::Embedding,
            score: c.clamp(0.0, 1.0),
        })
    }
}

/// Overlap coefficient of the two topics' `entity_keys` — what fraction
/// of the *smaller* entity set the two articles share. Uses overlap (not
/// Jaccard) because the sets are asymmetric: SEP articles name many
/// entities, Wikipedia pages few. This is the demoted name-cluster
/// meta-atom, reused as a feature.
pub struct SharedEntitiesSignal;
impl AlignmentSignal for SharedEntitiesSignal {
    fn kind(&self) -> BridgeSignal {
        BridgeSignal::SharedEntities
    }
    fn score(&self, left: &BridgeTopic, right: &BridgeTopic, _: &SignalContext) -> Option<SignalHit> {
        let o = overlap_coefficient(&left.entity_keys, &right.entity_keys);
        (o > 0.0).then_some(SignalHit {
            signal: BridgeSignal::SharedEntities,
            score: o,
        })
    }
}

/// SEP's named entities appearing as Wikipedia link-graph neighbours of
/// the candidate. The strong *structural* corroboration for SEP↔WP,
/// since Wikipedia pages are entity-sparse (direct entity overlap is
/// weak). Value pre-computed into [`SignalContext::co_neighbor_overlap`].
pub struct LinkGraphCoNeighborSignal;
impl AlignmentSignal for LinkGraphCoNeighborSignal {
    fn kind(&self) -> BridgeSignal {
        BridgeSignal::LinkGraphCoNeighbor
    }
    fn score(&self, _: &BridgeTopic, _: &BridgeTopic, ctx: &SignalContext) -> Option<SignalHit> {
        (ctx.co_neighbor_overlap > 0.0).then_some(SignalHit {
            signal: BridgeSignal::LinkGraphCoNeighbor,
            score: ctx.co_neighbor_overlap.clamp(0.0, 1.0),
        })
    }
}

/// The "two registers" signature: the two sides have *different*
/// dominant articulations (e.g. one argues a concept, the other
/// inventories it). A corroborating prior, not a primary matcher.
/// Generic over which side is which — no corpus or direction baked in.
pub struct ArticulationComplementaritySignal;
impl AlignmentSignal for ArticulationComplementaritySignal {
    fn kind(&self) -> BridgeSignal {
        BridgeSignal::ArticulationComplementarity
    }
    fn score(&self, left: &BridgeTopic, right: &BridgeTopic, _: &SignalContext) -> Option<SignalHit> {
        let (ld, rd) = (left.articulation.dominant(), right.articulation.dominant());
        if ld == rd {
            return None; // same register → not complementary
        }
        let s = (left.articulation.weight(ld) * right.articulation.weight(rd)).clamp(0.0, 1.0);
        Some(SignalHit {
            signal: BridgeSignal::ArticulationComplementarity,
            score: s,
        })
    }
}

/// Shared Wikidata QID — near-decisive when present (rare today).
pub struct WikidataAnchorSignal;
impl AlignmentSignal for WikidataAnchorSignal {
    fn kind(&self) -> BridgeSignal {
        BridgeSignal::WikidataAnchor
    }
    fn score(&self, _: &BridgeTopic, _: &BridgeTopic, ctx: &SignalContext) -> Option<SignalHit> {
        ctx.shared_wikidata_qid.then_some(SignalHit {
            signal: BridgeSignal::WikidataAnchor,
            score: 1.0,
        })
    }
}

// ── composite ─────────────────────────────────────────────────────

/// Which action the deterministic score implies for a candidate pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignmentBand {
    /// Deterministic confidence — emit a `same` edge, no model call.
    AutoSame,
    /// Send to the LLM adjudicator to type (same/broader/narrower/…).
    Uncertain,
    /// Below the floor — not a correspondence.
    Drop,
}

/// The folded score for one candidate pair.
#[derive(Debug, Clone)]
pub struct AlignmentScore {
    pub composite: f32,
    pub hits: Vec<SignalHit>,
    pub band: AlignmentBand,
}

impl AlignmentScore {
    pub fn signals(&self) -> Vec<BridgeSignal> {
        self.hits.iter().map(|h| h.signal).collect()
    }
}

/// Auto-`same` floor: composite at/above this is a deterministic match.
pub const TAU_SAME: f32 = 0.70;
/// Drop floor: composite below this is not a correspondence. Between the
/// two floors is the uncertain band the LLM adjudicator types.
pub const TAU_LOW: f32 = 0.38;

/// Per-signal weight in the composite. The five core signals sum to
/// 1.0. A shared Wikidata QID also contributes here, but its real force
/// is the decisive `AutoSame` override in [`SignalStack::evaluate`] —
/// shared-QID is identity, not a soft cue.
fn weight(sig: BridgeSignal) -> f32 {
    match sig {
        BridgeSignal::Embedding => 0.35,
        BridgeSignal::LinkGraphCoNeighbor => 0.25,
        BridgeSignal::NameMatch => 0.22,
        BridgeSignal::SharedEntities => 0.10,
        BridgeSignal::ArticulationComplementarity => 0.08,
        BridgeSignal::WikidataAnchor => 0.50,
    }
}

/// An ordered set of signals folded into one composite score + band.
pub struct SignalStack {
    signals: Vec<Box<dyn AlignmentSignal>>,
}

impl SignalStack {
    /// The v1 default stack — all six signals.
    pub fn default_stack() -> Self {
        Self {
            signals: vec![
                Box::new(NameMatchSignal),
                Box::new(EmbeddingSignal),
                Box::new(SharedEntitiesSignal),
                Box::new(LinkGraphCoNeighborSignal),
                Box::new(ArticulationComplementaritySignal),
                Box::new(WikidataAnchorSignal),
            ],
        }
    }

    pub fn from_signals(signals: Vec<Box<dyn AlignmentSignal>>) -> Self {
        Self { signals }
    }

    /// Score a candidate pair: run every signal, weight the hits, clamp
    /// to `0..1`, and band against the floors.
    pub fn evaluate(
        &self,
        left: &BridgeTopic,
        right: &BridgeTopic,
        ctx: &SignalContext,
    ) -> AlignmentScore {
        let mut hits = Vec::new();
        let mut composite = 0.0f32;
        for s in &self.signals {
            if let Some(hit) = s.score(left, right, ctx) {
                composite += weight(hit.signal) * hit.score;
                hits.push(hit);
            }
        }
        // A shared Wikidata QID is identity by definition — a decisive
        // override, not just a heavy weight. When it fires the pair is
        // `same` regardless of the soft composite (and we floor the
        // reported confidence so it reads consistently with the band).
        let wikidata_fired = hits
            .iter()
            .any(|h| h.signal == BridgeSignal::WikidataAnchor);
        let composite = if wikidata_fired {
            composite.max(TAU_SAME)
        } else {
            composite
        }
        .clamp(0.0, 1.0);
        let band = if wikidata_fired || composite >= TAU_SAME {
            AlignmentBand::AutoSame
        } else if composite >= TAU_LOW {
            AlignmentBand::Uncertain
        } else {
            AlignmentBand::Drop
        };
        AlignmentScore {
            composite,
            hits,
            band,
        }
    }
}

impl Default for SignalStack {
    fn default() -> Self {
        Self::default_stack()
    }
}

// ── pure helpers ──────────────────────────────────────────────────

fn tokens(s: &str) -> BTreeSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f32;
    let union = a.union(b).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

fn overlap_coefficient(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f32;
    inter / (a.len().min(b.len()) as f32)
}

/// Cosine similarity. `None` on length mismatch or a zero-norm vector.
fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return None;
    }
    Some(dot / (na.sqrt() * nb.sqrt()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream_axes::ArticulationVector;
    use std::collections::{BTreeMap, BTreeSet};

    fn topic(
        corpus: &str,
        title: &str,
        entity_keys: &[&str],
        articulation: ArticulationVector,
        embedding: Option<Vec<f32>>,
    ) -> BridgeTopic {
        BridgeTopic {
            corpus_id: corpus.into(),
            topic_id: "t".into(),
            title: title.into(),
            concept_text: title.into(),
            entity_keys: entity_keys.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>(),
            argument_names: Vec::new(),
            atom_profile: BTreeMap::new(),
            articulation,
            embedding,
        }
    }

    fn left_like(title: &str, keys: &[&str], emb: Option<Vec<f32>>) -> BridgeTopic {
        topic("sep-x", title, keys, ArticulationVector::new(0.10, 0.85, 0.05), emb)
    }
    fn right_like(title: &str, keys: &[&str], emb: Option<Vec<f32>>) -> BridgeTopic {
        topic("wikipedia", title, keys, ArticulationVector::new(0.80, 0.15, 0.05), emb)
    }

    #[test]
    fn name_match_scores_token_jaccard() {
        let s = left_like("Semantic Externalism", &[], None);
        let w = right_like("Semantic externalism", &[], None);
        let hit = NameMatchSignal.score(&s, &w, &SignalContext::default()).unwrap();
        assert_eq!(hit.score, 1.0); // same two tokens, case-insensitive
    }

    #[test]
    fn name_match_partial_overlap() {
        let s = left_like("Externalism About the Mind", &[], None);
        let w = right_like("Semantic externalism", &[], None);
        let hit = NameMatchSignal.score(&s, &w, &SignalContext::default()).unwrap();
        assert!(hit.score > 0.0 && hit.score < 1.0); // "externalism" shared only
    }

    #[test]
    fn embedding_signal_needs_both_vectors() {
        let s = left_like("X", &[], Some(vec![1.0, 0.0]));
        let w_no = right_like("X", &[], None);
        assert!(EmbeddingSignal.score(&s, &w_no, &SignalContext::default()).is_none());

        let w = right_like("X", &[], Some(vec![1.0, 0.0]));
        let hit = EmbeddingSignal.score(&s, &w, &SignalContext::default()).unwrap();
        assert!((hit.score - 1.0).abs() < 1e-6); // identical direction
    }

    #[test]
    fn shared_entities_uses_overlap_not_jaccard() {
        // WP page names 1 entity that IS in the SEP article's 4 — overlap
        // coefficient is 1.0 even though Jaccard would be 0.25.
        let s = left_like("Abduction", &["abduction", "peirce", "inference", "hypothesis"], None);
        let w = right_like("Abductive reasoning", &["abduction"], None);
        let hit = SharedEntitiesSignal.score(&s, &w, &SignalContext::default()).unwrap();
        assert_eq!(hit.score, 1.0);
    }

    #[test]
    fn articulation_complementarity_only_fires_on_the_signature() {
        let s = left_like("X", &[], None); // Argument-dominant
        let w = right_like("X", &[], None); // Inventory-dominant
        assert!(ArticulationComplementaritySignal
            .score(&s, &w, &SignalContext::default())
            .is_some());

        // Two Inventory-dominant topics → no complementarity.
        let w2 = right_like("X", &[], None);
        assert!(ArticulationComplementaritySignal
            .score(&w2, &w, &SignalContext::default())
            .is_none());
    }

    #[test]
    fn embedding_signal_falls_back_to_context_similarity() {
        // No vectors on either topic, but the orchestrator supplied the
        // ANN-derived similarity — the signal should still fire.
        let s = left_like("X", &[], None);
        let w = right_like("X", &[], None);
        let ctx = SignalContext {
            embedding_similarity: Some(0.82),
            ..Default::default()
        };
        let hit = EmbeddingSignal.score(&s, &w, &ctx).unwrap();
        assert!((hit.score - 0.82).abs() < 1e-6);
    }

    #[test]
    fn co_neighbor_signal_reads_context() {
        let s = left_like("X", &[], None);
        let w = right_like("X", &[], None);
        let ctx = SignalContext {
            co_neighbor_overlap: 0.6,
            ..Default::default()
        };
        let hit = LinkGraphCoNeighborSignal.score(&s, &w, &ctx).unwrap();
        assert!((hit.score - 0.6).abs() < 1e-6);
        assert!(LinkGraphCoNeighborSignal
            .score(&s, &w, &SignalContext::default())
            .is_none());
    }

    #[test]
    fn strong_pair_bands_auto_same() {
        let emb = Some(vec![1.0, 0.0, 0.0]);
        let s = left_like("Semantic Externalism", &["semantic externalism", "putnam"], emb.clone());
        let w = right_like("Semantic externalism", &["semantic externalism"], emb);
        let ctx = SignalContext {
            co_neighbor_overlap: 0.7,
            ..Default::default()
        };
        let score = SignalStack::default_stack().evaluate(&s, &w, &ctx);
        assert_eq!(score.band, AlignmentBand::AutoSame);
        assert!(score.composite >= TAU_SAME);
    }

    #[test]
    fn middling_pair_bands_uncertain() {
        // Decent embedding + name overlap, no structural corroboration.
        let s = left_like("Externalism About the Mind", &["externalism mind"], Some(vec![0.9, 0.4]));
        let w = right_like("Semantic externalism", &["semantic externalism"], Some(vec![0.8, 0.6]));
        let score = SignalStack::default_stack().evaluate(&s, &w, &SignalContext::default());
        assert_eq!(score.band, AlignmentBand::Uncertain);
    }

    #[test]
    fn unrelated_pair_drops() {
        let s = left_like("Abduction", &["abduction"], Some(vec![1.0, 0.0]));
        let w = right_like("Bee", &["bee"], Some(vec![0.0, 1.0])); // orthogonal embedding
        let score = SignalStack::default_stack().evaluate(&s, &w, &SignalContext::default());
        assert_eq!(score.band, AlignmentBand::Drop);
    }

    #[test]
    fn shared_qid_boosts_to_auto_same() {
        let s = left_like("Trope", &["trope"], None);
        let w = right_like("Trope (philosophy)", &["trope philosophy"], None);
        let ctx = SignalContext {
            shared_wikidata_qid: true,
            ..Default::default()
        };
        let score = SignalStack::default_stack().evaluate(&s, &w, &ctx);
        assert_eq!(score.band, AlignmentBand::AutoSame);
    }
}
