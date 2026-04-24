//! Tension candidate selection + Landing 4 placeholder.
//!
//! Landing 3 scope: produce a **candidate list** of atom pairs that
//! are plausibly in tension, using three cheap deterministic signals:
//!
//! 1. **Intra-cluster.** Two Claim atoms in the same Phase 2 claim
//!    cluster disagree on the same thematic axis by construction —
//!    clustering groups semantically similar claims, and tensions
//!    are the semantically similar but logically opposed pairs.
//! 2. **Entity-overlap.** Two Claims attributed to (or about) the
//!    same Entity often frame that entity from incompatible angles.
//!    Claim-vs-State pairs on the same entity surface
//!    saying-vs-doing mismatches (e.g. a claim that Alyosha is
//!    monastic while a state shows him in worldly action).
//! 3. **Embedding top-K.** For claims outside any cluster, the
//!    top-K nearest neighbours by embedding are a reasonable
//!    candidate pool. Reserved for Landing 4 when the runner
//!    plumbs an embed closure through this pass.
//!
//! Landing 4 will add the LLM classification pass that reads each
//! candidate, decides whether a real tension exists, and emits a
//! `Tension` edge with a `sub_question`. Until then this module
//! only produces the candidate list — no edges are materialised
//! on the atlas.

use serde::{Deserialize, Serialize};

use super::super::atoms::{AtomId, Claim, State};

/// How a candidate pair was discovered. Carried through to the
/// LLM classifier so it can weight intra-cluster pairs higher than
/// top-K pairs (intra-cluster comes with a pre-verified semantic
/// similarity floor; top-K is a looser signal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateSource {
    /// Both atoms are in the same Phase 2 cluster.
    IntraCluster,
    /// Both atoms share an entity attribution (claim-claim) or
    /// bind to the same entity (claim-state).
    EntityOverlap,
    /// Nearest-neighbour match via embedding cosine. Reserved —
    /// not produced by Landing 3's deterministic pass.
    EmbeddingTopK,
}

/// A pair of atoms flagged for LLM tension classification. The
/// classifier may decide this pair is not in tension — `candidate`
/// is weaker than `edge`. Both endpoints are Claim or State atom
/// ids; the classifier prompt branches on the pair's kinds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TensionCandidate {
    pub id: String,
    pub source_atom: AtomId,
    pub target_atom: AtomId,
    pub discovery: CandidateSource,
    /// Optional cluster id when `discovery == IntraCluster`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_id: Option<String>,
    /// Optional shared-entity id when
    /// `discovery == EntityOverlap`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_entity: Option<AtomId>,
}

/// Top-level candidates file. Not required to land on disk in
/// Landing 3 — the runner can hold candidates in memory until
/// Landing 4's LLM pass consumes them. The file format is pinned
/// here so both lands can roundtrip it cleanly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TensionCandidatesOutput {
    pub schema_version: String,
    pub candidates: Vec<TensionCandidate>,
}

impl TensionCandidatesOutput {
    pub const SCHEMA_VERSION: &'static str = "2.0";

    pub fn new(candidates: Vec<TensionCandidate>) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION.to_string(),
            candidates,
        }
    }
}

/// Inputs the selector reads. `claim_clusters` maps a cluster id to
/// the ordered list of claim atom ids that cluster contains.
/// Landing 3's `runner.rs::phase_6_atlas_tensions` assembles this
/// map from Phase 2's `Phase2AtlasOutput.claim_clusters`.
#[derive(Debug, Clone)]
pub struct CandidateSelectionInput<'a> {
    pub claims: &'a [Claim],
    pub states: &'a [State],
    /// `(cluster_id, [claim_atom_ids])` — claim clusters only.
    /// State/entity-state clusters don't drive tension candidacy
    /// directly; they surface via state-vs-claim entity overlap.
    pub claim_clusters: &'a [(String, Vec<AtomId>)],
}

/// Enumerate tension candidates across the three Landing 3 signals.
/// Idempotent + deterministic: same inputs → same ordered list →
/// same ids. Callers can dedup the result via
/// `candidate_pair_key` when multiple signals flag the same pair
/// (intra-cluster AND entity-overlap, for example).
pub fn select_candidates(input: CandidateSelectionInput<'_>) -> Vec<TensionCandidate> {
    let mut out = Vec::new();
    out.extend(select_intra_cluster(input.claim_clusters));
    out.extend(select_entity_overlap_claim_claim(input.claims));
    out.extend(select_entity_overlap_claim_state(input.claims, input.states));
    // Dedup pairs across signal sources — prefer IntraCluster >
    // EntityOverlap when the same (a, b) appears under both tags,
    // since the cluster pre-filter is a stronger prior.
    dedup_pairs(&mut out);
    // Stamp sequential ids.
    for (i, c) in out.iter_mut().enumerate() {
        c.id = format!("cand-{:04}", i + 1);
    }
    out
}

fn select_intra_cluster(
    clusters: &[(String, Vec<AtomId>)],
) -> Vec<TensionCandidate> {
    let mut out = Vec::new();
    for (cid, members) in clusters {
        for i in 0..members.len() {
            for j in (i + 1)..members.len() {
                out.push(TensionCandidate {
                    id: String::new(),
                    source_atom: members[i].clone(),
                    target_atom: members[j].clone(),
                    discovery: CandidateSource::IntraCluster,
                    cluster_id: Some(cid.clone()),
                    shared_entity: None,
                });
            }
        }
    }
    out
}

fn select_entity_overlap_claim_claim(claims: &[Claim]) -> Vec<TensionCandidate> {
    let mut out = Vec::new();
    // Bucket claims by their attribution. Only pairs within a bucket
    // of size ≥ 2 can produce entity-overlap candidates.
    let mut by_entity: std::collections::HashMap<&str, Vec<&Claim>> =
        std::collections::HashMap::new();
    for c in claims {
        if let Some(eid) = c.attributed_to.as_ref() {
            by_entity.entry(eid.as_str()).or_default().push(c);
        }
    }
    for (eid, group) in &by_entity {
        for i in 0..group.len() {
            for j in (i + 1)..group.len() {
                out.push(TensionCandidate {
                    id: String::new(),
                    source_atom: group[i].id.clone(),
                    target_atom: group[j].id.clone(),
                    discovery: CandidateSource::EntityOverlap,
                    cluster_id: None,
                    shared_entity: Some(AtomId::from_raw(eid.to_string())),
                });
            }
        }
    }
    out
}

fn select_entity_overlap_claim_state(
    claims: &[Claim],
    states: &[State],
) -> Vec<TensionCandidate> {
    // Match every claim attributed to E against every state whose
    // entity_id is E. A saying-vs-doing mismatch is the archetypal
    // novelistic tension (Alyosha asserts the primacy of brotherly
    // love in sec_0003 while sec_0007 shows him avoiding Ivan).
    let mut out = Vec::new();
    for c in claims {
        let Some(eid) = c.attributed_to.as_ref() else {
            continue;
        };
        for s in states {
            if &s.entity_id == eid {
                out.push(TensionCandidate {
                    id: String::new(),
                    source_atom: c.id.clone(),
                    target_atom: s.id.clone(),
                    discovery: CandidateSource::EntityOverlap,
                    cluster_id: None,
                    shared_entity: Some(eid.clone()),
                });
            }
        }
    }
    out
}

/// Canonical key for a candidate pair, ignoring source ordering.
/// Lets us collapse the same (a, b) discovered via multiple signals
/// into one record while preserving the stronger provenance.
fn candidate_pair_key(c: &TensionCandidate) -> (String, String) {
    let a = c.source_atom.as_str().to_string();
    let b = c.target_atom.as_str().to_string();
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

fn source_rank(s: CandidateSource) -> u8 {
    match s {
        CandidateSource::IntraCluster => 0,
        CandidateSource::EntityOverlap => 1,
        CandidateSource::EmbeddingTopK => 2,
    }
}

/// In-place dedup: if the same unordered pair appears under
/// multiple discovery sources, keep the stronger source per
/// `source_rank`. Preserves first-seen order within rank.
fn dedup_pairs(cands: &mut Vec<TensionCandidate>) {
    use std::collections::HashMap;
    let mut keep: HashMap<(String, String), TensionCandidate> = HashMap::new();
    let mut order: Vec<(String, String)> = Vec::new();
    for c in cands.drain(..) {
        let k = candidate_pair_key(&c);
        match keep.get(&k) {
            Some(existing) if source_rank(existing.discovery) <= source_rank(c.discovery) => {
                // Existing is at least as strong — keep it.
            }
            _ => {
                if !keep.contains_key(&k) {
                    order.push(k.clone());
                }
                keep.insert(k, c);
            }
        }
    }
    for k in order {
        if let Some(c) = keep.remove(&k) {
            cands.push(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::pipeline::atlas::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EpistemicStatus, StateType,
    };
    use super::super::super::atoms::{ChunkRef, SectionRange};

    fn claim(id: u32, attributed_to: Option<u32>) -> Claim {
        Claim {
            id: AtomId::from_raw(format!("claim-{id:04}")),
            content: format!("claim {id}"),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: vec![ChunkRef::new("sec_0001", None)],
            attributed_to: attributed_to
                .map(|e| AtomId::from_raw(format!("entity-{e:04}"))),
            confidence: Some(1.0),
            enrichment_depth: EnrichmentDepth::Extracted,
        }
    }

    fn state(id: u32, owner: u32) -> State {
        State {
            id: AtomId::from_raw(format!("state-{id:04}")),
            entity_id: AtomId::from_raw(format!("entity-{owner:04}")),
            label: format!("state {id}"),
            state_type: StateType::Other("unknown".into()),
            evidence: Vec::new(),
            section_range: SectionRange {
                start: "sec_0001".into(),
                end: "sec_0001".into(),
            },
            confidence: Some(1.0),
            enrichment_depth: EnrichmentDepth::Extracted,
        }
    }

    #[test]
    fn intra_cluster_enumerates_upper_triangle_pairs() {
        let cluster = (
            "cluster-01".into(),
            vec![
                AtomId::from_raw("claim-0001"),
                AtomId::from_raw("claim-0002"),
                AtomId::from_raw("claim-0003"),
            ],
        );
        let out = select_candidates(CandidateSelectionInput {
            claims: &[],
            states: &[],
            claim_clusters: &[cluster],
        });
        // 3 claims → C(3,2) = 3 unique pairs.
        assert_eq!(out.len(), 3);
        assert!(out
            .iter()
            .all(|c| matches!(c.discovery, CandidateSource::IntraCluster)));
        assert!(out.iter().all(|c| c.cluster_id.as_deref() == Some("cluster-01")));
    }

    #[test]
    fn entity_overlap_claim_claim_groups_by_attribution() {
        // Two claims attributed to entity-0001 (should pair), one
        // attributed to entity-0002 (no pair), one unattributed.
        let claims = vec![
            claim(1, Some(1)),
            claim(2, Some(1)),
            claim(3, Some(2)),
            claim(4, None),
        ];
        let out = select_candidates(CandidateSelectionInput {
            claims: &claims,
            states: &[],
            claim_clusters: &[],
        });
        // Exactly one pair: claim-0001 ↔ claim-0002.
        assert_eq!(out.len(), 1);
        let c = &out[0];
        assert_eq!(c.source_atom.as_str(), "claim-0001");
        assert_eq!(c.target_atom.as_str(), "claim-0002");
        assert!(matches!(c.discovery, CandidateSource::EntityOverlap));
        assert_eq!(
            c.shared_entity.as_ref().map(|a| a.as_str()),
            Some("entity-0001")
        );
    }

    #[test]
    fn entity_overlap_claim_state_matches_same_entity() {
        // One claim attributed to entity-0001, two states on
        // entity-0001 → two claim↔state candidates.
        let claims = vec![claim(1, Some(1))];
        let states = vec![state(1, 1), state(2, 1), state(3, 2)];
        let out = select_candidates(CandidateSelectionInput {
            claims: &claims,
            states: &states,
            claim_clusters: &[],
        });
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|c| c.source_atom.as_str() == "claim-0001"));
        let targets: std::collections::HashSet<&str> =
            out.iter().map(|c| c.target_atom.as_str()).collect();
        assert!(targets.contains("state-0001"));
        assert!(targets.contains("state-0002"));
        assert!(!targets.contains("state-0003"));
    }

    #[test]
    fn dedup_prefers_intra_cluster_over_entity_overlap_for_same_pair() {
        // Same two claims appear in one cluster AND share an
        // attribution. The result should be one candidate tagged
        // IntraCluster (the stronger prior).
        let claims = vec![claim(1, Some(1)), claim(2, Some(1))];
        let cluster = (
            "cluster-01".into(),
            vec![
                AtomId::from_raw("claim-0001"),
                AtomId::from_raw("claim-0002"),
            ],
        );
        let out = select_candidates(CandidateSelectionInput {
            claims: &claims,
            states: &[],
            claim_clusters: &[cluster],
        });
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0].discovery, CandidateSource::IntraCluster));
    }

    #[test]
    fn candidates_get_sequential_ids() {
        let claims = vec![claim(1, Some(1)), claim(2, Some(1)), claim(3, Some(1))];
        let out = select_candidates(CandidateSelectionInput {
            claims: &claims,
            states: &[],
            claim_clusters: &[],
        });
        let ids: Vec<&str> = out.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["cand-0001", "cand-0002", "cand-0003"]);
    }
}
