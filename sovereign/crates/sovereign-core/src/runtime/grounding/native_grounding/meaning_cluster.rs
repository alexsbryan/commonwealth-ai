// SPDX-License-Identifier: AGPL-3.0-or-later
//! H2 deliverable 2 — clustering k sampled values by MEANING, and the two
//! statistics over the clusters.
//!
//! `NATIVE_GROUNDING.md` §5 H2 step 2: *"Cluster the k values by meaning
//! equivalence, cheapest instrument first: (a) the deterministic kernel
//! (`value_present` normalization, stopword-stripped AND-match) merges
//! exact/near-exact values; (b) survivors are merged by bidirectional
//! entailment via the reranker margin — `margin(a→b)` and `margin(b→a)` both
//! above the clustering floor collapses a pair …; embed-slot cosine is the
//! tie-breaker, not the decider, because cosine measures topic — the same flaw
//! that killed `top_cosine` must not be smuggled into the clusterer."*
//!
//! **Zero runtime callers, by construction** — same terms as its sibling
//! modules. This is measurement surface for the H2 gate.
//!
//! # The two rungs, cheapest first
//!
//! | Rung | Instrument | Cost | Merges |
//! |---|---|---|---|
//! | (a) | `value_present_in_chunks` both ways | free, no model | exact / near-exact values |
//! | (b) | `margin(a→b)` AND `margin(b→a)` ≥ floor | ~23 ms/pair, ≤ C(5,2)=10 pairs | paraphrases |
//! | tie-break | embed cosine | one embed call | NEVER a merge on its own — see below |
//!
//! Rung (a) is not a heuristic bolted on to save money; it is the incumbent's
//! own shipped presence kernel (`value_presence.rs:152`) used bidirectionally.
//! Two values each of whose significant words appear in the other are the same
//! value under exactly the definition the production grounding gate already
//! uses. Reusing it means the clusterer and the gate cannot disagree about what
//! "the same value" means (principle 8).
//!
//! # Why cosine cannot decide a merge
//!
//! §5 H2 says this verbatim and it is the one rule in this file with a test
//! whose failure would be a design violation rather than a bug: *cosine
//! measures topic*, and H1 already measured what that costs — `top_cosine`
//! scored 0.7994 AUROC against the reranker margin's 0.8990 on 4,207
//! calibration pairs (`sovereign/bench/calibration/h1/FINDINGS.md`). Two
//! different answers to the same question are maximally on-topic with each
//! other, so a cosine-decided clusterer would merge exactly the disagreements
//! H2 exists to see.
//!
//! So the tie-break is structurally incapable of creating a merge: it is
//! consulted ONLY when the two margins straddle the floor (one direction
//! clears it, the other does not), and it can only break that tie. When
//! NEITHER direction clears the floor there is no tie, cosine is never asked,
//! and no cosine value — not even 1.0 — can merge the pair.
//! [`no_cosine_value_can_merge_a_pair_the_margins_rejected`] pins that.
//!
//! # No threshold lives here
//!
//! The clustering floor is a parameter of [`cluster_values`], never a constant
//! in this file. It is calibrated in the H2 harness against a value-equivalence
//! set and committed beside the code that reads it (principle 2, §7.1's "a
//! threshold with no committed curve fails review").

use serde::{Deserialize, Serialize};

use super::sentence_sweep::SentenceScorer;
use crate::runtime::value_present_in_chunks;

/// One meaning-cluster: the sample indices that assert the same value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeaningCluster {
    /// Indices into the draw's sample list, ascending.
    pub members: Vec<usize>,
    /// The cluster's representative surface form — the value of its
    /// lowest-indexed member, or `None` for the no-value cluster. Chosen by
    /// index rather than by length or centrality so two runs over the same
    /// draw produce byte-identical clusters (§18.4 — the instrument must be
    /// reproducible before its output is a result).
    pub representative: Option<String>,
}

/// Which rung merged a pair. Glassbox: every merge names its decider
/// (principle 1, and §6's `decided_by` in miniature).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeRung {
    /// Rung (a): the deterministic presence kernel, both directions.
    DetKernel,
    /// Rung (b): bidirectional reranker entailment, both margins ≥ floor.
    BidirectionalEntailment,
    /// Rung (b) with the cosine tie-break: the margins straddled the floor and
    /// cosine resolved it. Counted separately because a run where this
    /// dominates is a run whose floor is mis-calibrated, and that must be
    /// visible in the artifact rather than inferred.
    EntailmentCosineTiebreak,
    /// Both samples asserted no value.
    NoValue,
}

/// One merge, recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeRecord {
    /// The two sample indices, lower first.
    pub a: usize,
    /// The higher sample index.
    pub b: usize,
    /// Which rung merged them.
    pub rung: MergeRung,
    /// `margin(a→b)`, when rung (b) ran.
    pub margin_ab: Option<f32>,
    /// `margin(b→a)`, when rung (b) ran.
    pub margin_ba: Option<f32>,
    /// The cosine the tie-break saw, when it ran.
    pub cosine: Option<f32>,
}

/// The clustering of one draw, plus H2's two statistics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClusterResult {
    /// Clusters, ordered by their lowest member index.
    pub clusters: Vec<MeaningCluster>,
    /// `semantic_entropy = −Σ_c p(c)·log p(c)` with count-based
    /// `p(c) = |c|/k` (§5 H2's k=5 form; the probability-weighted upgrade
    /// waits on H5's logprobs). Natural log, so the range is `[0, ln k]`:
    /// 0 = unanimous, `ln k` = every sample its own meaning.
    pub semantic_entropy: f32,
    /// `agreement = |largest cluster| / k` — the degenerate cheap statistic
    /// H2 reports beside entropy so the complexity rule (§7.3: entropy must
    /// beat agreement by ≥0.02 AUROC or the cheaper statistic ships) can be
    /// evaluated from one artifact.
    pub agreement: f32,
    /// The k this was computed over. Carried because both statistics are
    /// meaningless without it — an entropy of 1.1 is near-maximal at k=3 and
    /// middling at k=8.
    pub k: usize,
    /// Every merge, with its decider. Ordered.
    pub merges: Vec<MergeRecord>,
    /// Reranker pairs actually scored. The cost half of §7.3's "at <20% of
    /// its per-turn judge cost" — reported, never estimated.
    pub pairs_scored: usize,
}

impl ClusterResult {
    /// Merges attributed to a given rung.
    pub fn merges_by(&self, rung: MergeRung) -> usize {
        self.merges.iter().filter(|m| m.rung == rung).count()
    }
}

/// The embedding tie-breaker's seam.
///
/// Deliberately a separate trait from [`SentenceScorer`] even though both
/// return `f32`s: they are different instruments with different semantics
/// (a cross-encoder margin is unbounded and directional; a cosine is bounded
/// and symmetric), and giving them one trait would make it easy to pass the
/// wrong one. The clusterer takes cosine as `Option` because it is genuinely
/// optional — every gate in this order runs with `None`.
#[async_trait::async_trait]
pub trait CosineTiebreaker: Send + Sync {
    /// Cosine similarity of two values, in `[-1, 1]`.
    async fn cosine(&self, a: &str, b: &str) -> Result<f32, String>;
}

/// Rung (a): do these two values assert the same thing under the shipped
/// deterministic presence kernel?
///
/// Bidirectional on purpose. `value_present_in_chunks(a, [b])` alone is
/// containment, not equality — "Verloc" is present in "Mr Verloc of Brett
/// Street" but the reverse is false, and merging on one direction would fold
/// every specific answer into every vaguer one, which is precisely the
/// `partially_present` failure the banks are built to catch.
pub fn det_equivalent(a: &str, b: &str) -> bool {
    value_present_in_chunks(a, std::slice::from_ref(&b.to_string()))
        && value_present_in_chunks(b, std::slice::from_ref(&a.to_string()))
}

/// The entailment query posed to the cross-encoder for rung (b).
///
/// One place builds it, so `margin(a→b)` and `margin(b→a)` are the same
/// question asked twice with the arguments swapped, and not two subtly
/// different questions (principle 8).
pub fn entailment_query(from: &str) -> String {
    format!("Does this answer mean the same as: {from}")
}

/// Cluster `k` sampled values by meaning and compute H2's two statistics.
///
/// `values[i] == None` means sample `i` asserted no value (a decline). All such
/// samples form ONE cluster: k declines are unanimous agreement that the
/// evidence does not say, which is a meaning, not an absence of one.
///
/// `floor` is the clustering floor from the committed calibration curve.
/// `cosine` is the optional tie-breaker; passing `None` disables the tie-break
/// entirely and changes no merge that would otherwise have happened on rung (b)
/// alone.
///
/// Deterministic: the same `(values, floor)` and a deterministic scorer produce
/// byte-identical output, including merge order. Pairs are visited in ascending
/// `(a, b)` order and clusters carry ascending members.
pub async fn cluster_values(
    values: &[Option<String>],
    scorer: &dyn SentenceScorer,
    cosine: Option<&dyn CosineTiebreaker>,
    floor: f32,
) -> Result<ClusterResult, String> {
    let k = values.len();
    if k == 0 {
        return Err(
            "cluster_values got zero samples — an entropy over no draw is not a statistic"
                .to_string(),
        );
    }

    // Union-find over sample indices. Small k, so the naive form is right.
    let mut parent: Vec<usize> = (0..k).collect();
    fn find(parent: &mut Vec<usize>, mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }

    let mut merges: Vec<MergeRecord> = Vec::new();
    let mut pairs_scored = 0usize;

    // ── Rung (a) + the no-value cluster: free, no model ─────────────
    for a in 0..k {
        for b in (a + 1)..k {
            if find(&mut parent, a) == find(&mut parent, b) {
                continue;
            }
            let merged = match (&values[a], &values[b]) {
                (None, None) => Some(MergeRung::NoValue),
                (Some(va), Some(vb)) if det_equivalent(va, vb) => Some(MergeRung::DetKernel),
                _ => None,
            };
            if let Some(rung) = merged {
                let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                parent[rb] = ra;
                merges.push(MergeRecord {
                    a,
                    b,
                    rung,
                    margin_ab: None,
                    margin_ba: None,
                    cosine: None,
                });
            }
        }
    }

    // ── Rung (b): bidirectional entailment over SURVIVING pairs ─────
    //
    // Only cross-cluster pairs are scored, and only pairs where both sides
    // assert a value. That is what "survivors" means in §5 H2 and it is what
    // keeps the reranker cost at "at most C(5,2)=10 pairs" rather than always
    // 10.
    for a in 0..k {
        for b in (a + 1)..k {
            if find(&mut parent, a) == find(&mut parent, b) {
                continue;
            }
            let (Some(va), Some(vb)) = (&values[a], &values[b]) else {
                // A value and a decline are different meanings. No instrument
                // is asked; there is nothing ambiguous about it.
                continue;
            };
            let m_ab = *scorer
                .score(&entailment_query(va), std::slice::from_ref(vb))
                .await?
                .first()
                .ok_or_else(|| {
                    format!("scorer returned no margin for the pair ({a}, {b}) a→b")
                })?;
            let m_ba = *scorer
                .score(&entailment_query(vb), std::slice::from_ref(va))
                .await?
                .first()
                .ok_or_else(|| {
                    format!("scorer returned no margin for the pair ({a}, {b}) b→a")
                })?;
            pairs_scored += 2;

            let ab_clears = m_ab >= floor;
            let ba_clears = m_ba >= floor;
            let (do_merge, rung, cos) = if ab_clears && ba_clears {
                // Both directions entail. Cosine is not consulted — it has
                // nothing to add and §5 H2 does not let it subtract.
                (true, MergeRung::BidirectionalEntailment, None)
            } else if ab_clears != ba_clears {
                // The straddle: exactly one direction clears. THE ONLY place
                // cosine is asked anything.
                match cosine {
                    Some(c) => {
                        let cv = c.cosine(va, vb).await?;
                        (cv >= COSINE_TIEBREAK_FLOOR, MergeRung::EntailmentCosineTiebreak, Some(cv))
                    }
                    // No tie-breaker configured: a straddle does not merge.
                    // Refusing to merge is the conservative direction — it
                    // splits a cluster, which RAISES entropy, so the absence
                    // of an optional instrument can never manufacture the
                    // agreement H2 is trying to detect.
                    None => (false, MergeRung::EntailmentCosineTiebreak, None),
                }
            } else {
                // Neither direction clears. No tie to break. Cosine is never
                // asked and cannot merge this pair at any value.
                (false, MergeRung::BidirectionalEntailment, None)
            };

            if do_merge {
                let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                parent[rb] = ra;
                merges.push(MergeRecord {
                    a,
                    b,
                    rung,
                    margin_ab: Some(m_ab),
                    margin_ba: Some(m_ba),
                    cosine: cos,
                });
            }
        }
    }

    // ── Materialise clusters, ordered ───────────────────────────────
    let mut roots: Vec<usize> = Vec::new();
    let mut buckets: Vec<Vec<usize>> = Vec::new();
    for i in 0..k {
        let r = find(&mut parent, i);
        match roots.iter().position(|&x| x == r) {
            Some(p) => buckets[p].push(i),
            None => {
                roots.push(r);
                buckets.push(vec![i]);
            }
        }
    }
    let clusters: Vec<MeaningCluster> = buckets
        .into_iter()
        .map(|members| MeaningCluster {
            representative: values[members[0]].clone(),
            members,
        })
        .collect();

    let (semantic_entropy, agreement) = statistics(&clusters, k);

    Ok(ClusterResult {
        clusters,
        semantic_entropy,
        agreement,
        k,
        merges,
        pairs_scored,
    })
}

/// The cosine a tie-break must clear to resolve a straddle in favour of
/// merging.
///
/// Not a clustering threshold and deliberately not calibrated: it decides
/// nothing on its own (see the module doc), it only breaks a tie the margins
/// already declared ambiguous. Set at the conventional "same-topic" cosine so a
/// straddled pair merges only when the embedder also thinks they are close.
pub const COSINE_TIEBREAK_FLOOR: f32 = 0.85;

/// `−Σ p log p` and `max|c|/k` over a clustering.
///
/// Split out and public because both statistics must be computable from a
/// frozen clustering with no model — that is what makes the H2 gate replayable
/// from committed scores the way H1's and H4's are.
pub fn statistics(clusters: &[MeaningCluster], k: usize) -> (f32, f32) {
    if k == 0 || clusters.is_empty() {
        return (0.0, 0.0);
    }
    let kf = k as f32;
    let mut entropy = 0.0f32;
    let mut largest = 0usize;
    for c in clusters {
        let n = c.members.len();
        if n == 0 {
            continue;
        }
        largest = largest.max(n);
        let p = n as f32 / kf;
        entropy -= p * p.ln();
    }
    // A single cluster gives −1·ln(1) = −0.0; normalise the sign so a frozen
    // artifact never carries "-0" and two runs cannot differ in its rendering.
    if entropy == 0.0 {
        entropy = 0.0;
    }
    (entropy, largest as f32 / kf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scorer whose margins are a fixed table, so every clustering test is a
    /// pure function. Keyed by the ORDERED pair of value strings, so a test can
    /// make entailment asymmetric — which is the whole point of scoring both
    /// directions.
    struct TableScorer {
        table: Vec<((String, String), f32)>,
        default: f32,
        calls: std::sync::Mutex<usize>,
    }

    impl TableScorer {
        fn new(pairs: &[(&str, &str, f32)], default: f32) -> Self {
            Self {
                table: pairs
                    .iter()
                    .map(|(a, b, m)| ((a.to_string(), b.to_string()), *m))
                    .collect(),
                default,
                calls: std::sync::Mutex::new(0),
            }
        }
        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait::async_trait]
    impl SentenceScorer for TableScorer {
        async fn score(&self, query: &str, docs: &[String]) -> Result<Vec<f32>, String> {
            *self.calls.lock().unwrap() += 1;
            // `query` is `entailment_query(from)`; recover `from`.
            let from = query
                .strip_prefix("Does this answer mean the same as: ")
                .unwrap_or(query)
                .to_string();
            Ok(docs
                .iter()
                .map(|to| {
                    self.table
                        .iter()
                        .find(|((a, b), _)| a == &from && b == to)
                        .map(|(_, m)| *m)
                        .unwrap_or(self.default)
                })
                .collect())
        }
    }

    /// A tie-breaker that always answers the same cosine.
    struct FixedCosine(f32, std::sync::Mutex<usize>);
    impl FixedCosine {
        fn new(v: f32) -> Self {
            Self(v, std::sync::Mutex::new(0))
        }
        fn calls(&self) -> usize {
            *self.1.lock().unwrap()
        }
    }
    #[async_trait::async_trait]
    impl CosineTiebreaker for FixedCosine {
        async fn cosine(&self, _a: &str, _b: &str) -> Result<f32, String> {
            *self.1.lock().unwrap() += 1;
            Ok(self.0)
        }
    }

    fn vals(v: &[Option<&str>]) -> Vec<Option<String>> {
        v.iter().map(|x| x.map(|s| s.to_string())).collect()
    }

    // ── THE DESIGN RULE: cosine never decides a merge ───────────────

    #[tokio::test]
    async fn no_cosine_value_can_merge_a_pair_the_margins_rejected() {
        // §5 H2, verbatim: "embed-slot cosine is the tie-breaker, not the
        // decider, because cosine measures topic — the same flaw that
        // killed `top_cosine` must not be smuggled into the clusterer."
        //
        // Two genuinely different answers to the same question are maximally
        // on-topic with each other. Here BOTH margins are far below the
        // floor and cosine is a perfect 1.0. If cosine could decide, these
        // would merge, entropy would read 0, and H2 would report unanimity
        // on a draw that disagreed.
        let scorer = TableScorer::new(&[], -9.0);
        let cos = FixedCosine::new(1.0);
        let r = cluster_values(
            &vals(&[Some("Severin Quenholt"), Some("Lessa Pellow")]),
            &scorer,
            Some(&cos),
            0.0,
        )
        .await
        .unwrap();
        assert_eq!(r.clusters.len(), 2, "a perfect cosine must not merge them");
        assert_eq!(
            cos.calls(),
            0,
            "cosine must not even be CONSULTED when neither direction clears \
             the floor — there is no tie to break"
        );
        assert_eq!(r.agreement, 0.5);
    }

    #[tokio::test]
    async fn cosine_only_breaks_a_genuine_straddle() {
        // One direction clears, the other does not — the only ambiguous
        // case. Here and only here cosine is asked.
        let scorer = TableScorer::new(
            &[("A", "B", 5.0), ("B", "A", -5.0)],
            -9.0,
        );
        let cos = FixedCosine::new(0.99);
        let r = cluster_values(&vals(&[Some("A"), Some("B")]), &scorer, Some(&cos), 0.0)
            .await
            .unwrap();
        assert_eq!(cos.calls(), 1, "the straddle is the tie-break's only job");
        assert_eq!(r.clusters.len(), 1);
        assert_eq!(r.merges_by(MergeRung::EntailmentCosineTiebreak), 1);
    }

    #[tokio::test]
    async fn a_straddle_with_no_tiebreaker_splits_rather_than_merges() {
        // Absence of the optional instrument must be conservative: splitting
        // RAISES entropy, so a missing embedder can never manufacture
        // agreement (§18.3 — absence is reported, never defaulted into the
        // convenient direction).
        let scorer = TableScorer::new(&[("A", "B", 5.0), ("B", "A", -5.0)], -9.0);
        let r = cluster_values(&vals(&[Some("A"), Some("B")]), &scorer, None, 0.0)
            .await
            .unwrap();
        assert_eq!(r.clusters.len(), 2);
        assert!(r.semantic_entropy > 0.0);
    }

    // ── Rung (a): the deterministic kernel ──────────────────────────

    #[tokio::test]
    async fn rung_a_merges_exact_and_near_exact_values_without_a_model() {
        let scorer = TableScorer::new(&[], -9.0);
        let r = cluster_values(
            &vals(&[
                Some("Severin Quenholt"),
                Some("severin  quenholt"),
                Some("Mr Severin Quenholt"),
            ]),
            &scorer,
            None,
            0.0,
        )
        .await
        .unwrap();
        assert_eq!(r.clusters.len(), 1, "case, spacing and an honorific are not meanings");
        assert_eq!(
            scorer.calls(),
            0,
            "rung (a) must settle these for free — the reranker is the EXPENSIVE rung"
        );
        assert_eq!(r.pairs_scored, 0);
        assert_eq!(r.agreement, 1.0);
        assert_eq!(r.semantic_entropy, 0.0);
    }

    #[tokio::test]
    async fn det_equivalence_is_bidirectional_not_containment() {
        // "Verloc" is contained in "Mr Verloc of Brett Street" but not the
        // reverse. Merging on one direction would fold every specific answer
        // into every vaguer one — the `partially_present` failure shape.
        assert!(!det_equivalent("Verloc", "Verloc of Brett Street"));
        assert!(det_equivalent("Karl Yundt", "karl yundt"));
    }

    // ── Rung (b): bidirectional entailment ──────────────────────────

    #[tokio::test]
    async fn rung_b_needs_both_directions() {
        let one_way = TableScorer::new(&[("A", "B", 9.0), ("B", "A", -9.0)], -9.0);
        let r = cluster_values(&vals(&[Some("A"), Some("B")]), &one_way, None, 0.0)
            .await
            .unwrap();
        assert_eq!(r.clusters.len(), 2, "one-way entailment is not equivalence");

        let both = TableScorer::new(&[("A", "B", 9.0), ("B", "A", 9.0)], -9.0);
        let r2 = cluster_values(&vals(&[Some("A"), Some("B")]), &both, None, 0.0)
            .await
            .unwrap();
        assert_eq!(r2.clusters.len(), 1);
        assert_eq!(r2.merges_by(MergeRung::BidirectionalEntailment), 1);
        assert_eq!(r2.pairs_scored, 2, "two directions, two scored pairs");
    }

    // ── Declines ────────────────────────────────────────────────────

    #[tokio::test]
    async fn all_declines_are_one_cluster_not_k_clusters() {
        // k declines are unanimous agreement that the evidence does not say.
        // Splitting them would report maximal entropy on the most confident
        // honest outcome there is.
        let scorer = TableScorer::new(&[], -9.0);
        let r = cluster_values(&vals(&[None, None, None]), &scorer, None, 0.0)
            .await
            .unwrap();
        assert_eq!(r.clusters.len(), 1);
        assert_eq!(r.agreement, 1.0);
        assert_eq!(r.semantic_entropy, 0.0);
        assert_eq!(r.merges_by(MergeRung::NoValue), 2);
        assert_eq!(scorer.calls(), 0, "a decline needs no model to recognise");
    }

    #[tokio::test]
    async fn a_decline_and_a_value_are_different_meanings() {
        let scorer = TableScorer::new(&[], 9.0); // scorer would merge anything
        let r = cluster_values(&vals(&[None, Some("Quenholt")]), &scorer, None, 0.0)
            .await
            .unwrap();
        assert_eq!(r.clusters.len(), 2);
        assert_eq!(
            scorer.calls(),
            0,
            "no instrument is asked — a value and a decline are not ambiguous"
        );
    }

    // ── The statistics ──────────────────────────────────────────────

    #[test]
    fn entropy_and_agreement_on_the_shapes_the_spec_names() {
        // §5 H2: "a 3-1-1 split and a 3-2 split have equal agreement but
        // different entropy, and that tail structure is where
        // hedge-vs-abstain lives." That sentence is the reason entropy is
        // the primary statistic, so it gets a test.
        let c = |sizes: &[usize]| -> Vec<MeaningCluster> {
            let mut next = 0;
            sizes
                .iter()
                .map(|&n| {
                    let members: Vec<usize> = (next..next + n).collect();
                    next += n;
                    MeaningCluster {
                        members,
                        representative: None,
                    }
                })
                .collect()
        };
        let (e_311, a_311) = statistics(&c(&[3, 1, 1]), 5);
        let (e_32, a_32) = statistics(&c(&[3, 2]), 5);
        assert_eq!(a_311, a_32, "equal agreement — the spec's premise");
        assert!(
            e_311 > e_32,
            "different entropy — 3-1-1 ({e_311}) must exceed 3-2 ({e_32}), or \
             the primary statistic sees nothing the cheap one misses"
        );

        // Range: 0 at unanimity, ln k at full divergence.
        let (e_one, a_one) = statistics(&c(&[5]), 5);
        assert_eq!(e_one, 0.0);
        assert_eq!(a_one, 1.0);
        let (e_all, a_all) = statistics(&c(&[1, 1, 1, 1, 1]), 5);
        assert!((e_all - 5f32.ln()).abs() < 1e-5, "full divergence is ln k");
        assert_eq!(a_all, 0.2);
    }

    // ── Determinism ─────────────────────────────────────────────────

    #[tokio::test]
    async fn clustering_is_byte_stable_across_repeats() {
        // The determinism pin (§18.4). Runs with NO model on disk, by
        // construction — the scorer is a table.
        // One draw exercising all three outcomes at once: a rung-(a) merge
        // (case), a rung-(b) merge (paraphrase), and a lone decline.
        let v = vals(&[
            Some("Karl Yundt"),
            Some("Ossipon"),
            Some("karl yundt"),
            None,
            Some("the Professor"),
        ]);
        let mk = || {
            TableScorer::new(
                &[
                    ("Ossipon", "the Professor", 9.0),
                    ("the Professor", "Ossipon", 9.0),
                ],
                -9.0,
            )
        };
        let r1 = cluster_values(&v, &mk(), None, 0.0).await.unwrap();
        let r2 = cluster_values(&v, &mk(), None, 0.0).await.unwrap();
        assert_eq!(r1, r2);
        assert_eq!(
            serde_json::to_string(&r1).unwrap(),
            serde_json::to_string(&r2).unwrap(),
            "the serialized artifact must be byte-identical, not merely equal"
        );
        // And the clustering is the one we meant.
        assert_eq!(r1.clusters.len(), 3);
        assert_eq!(r1.clusters[0].members, vec![0, 2], "rung (a): case");
        assert_eq!(r1.clusters[1].members, vec![1, 4], "rung (b): paraphrase");
        assert_eq!(r1.clusters[2].members, vec![3], "the decline stands alone");
        assert_eq!(r1.merges_by(MergeRung::DetKernel), 1);
        assert_eq!(r1.merges_by(MergeRung::BidirectionalEntailment), 1);
    }

    #[tokio::test]
    async fn an_empty_draw_is_refused_not_scored_as_zero_entropy() {
        // Zero entropy is the reading for "unanimous". An empty draw must
        // never produce it (§18.3).
        let scorer = TableScorer::new(&[], 0.0);
        let err = cluster_values(&[], &scorer, None, 0.0)
            .await
            .expect_err("an empty draw is not a unanimous one");
        assert!(err.contains("zero samples"), "{err}");
    }

    #[tokio::test]
    async fn the_floor_is_a_parameter_and_it_bites() {
        // The floor must actually change the clustering, or it is not a
        // calibrated threshold — it is decoration (§18.1: a check with no
        // failing input you can name).
        let v = vals(&[Some("A"), Some("B")]);
        let scorer = || TableScorer::new(&[("A", "B", 3.0), ("B", "A", 3.0)], -9.0);
        let low = cluster_values(&v, &scorer(), None, 1.0).await.unwrap();
        assert_eq!(low.clusters.len(), 1, "floor 1.0 < margins 3.0 → merge");
        let high = cluster_values(&v, &scorer(), None, 5.0).await.unwrap();
        assert_eq!(high.clusters.len(), 2, "floor 5.0 > margins 3.0 → split");
    }
}
