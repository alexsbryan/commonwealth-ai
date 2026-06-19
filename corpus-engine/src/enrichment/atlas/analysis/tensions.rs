// SPDX-License-Identifier: AGPL-3.0-or-later
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

use super::super::atoms::{AtomId, Claim, Entity, State};
use crate::enrichment::pipeline::atlas::EntityType;

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
    /// Both claims are attributed to *different* positions but their
    /// evidence lands in the same Phase-1 section. Two positions
    /// discussed in the same chapter are by construction in dialogue
    /// — this signal catches structural disagreement that doesn't
    /// surface via shared entity mentions in claim content.
    ChunkCooccurrence,
    /// Nearest-neighbour match via embedding cosine. Produced by
    /// [`select_embedding_topk`] under the
    /// [`TensionStrategy::EmbeddingTopK`] strategy (custom-ontology
    /// corpora); the deterministic graph pass does not emit it.
    EmbeddingTopK,
}

/// Which Phase-6 candidate-selection strategy a pipeline uses.
///
/// [`TensionStrategy::Graph`] is the literary/philosophy default — the
/// cluster + entity-overlap + chunk-co-occurrence signals, tuned for
/// *within-document* narrative tensions. Recipe-ontology (`custom_atlas`)
/// corpora — governance rule-sets, policy docs — are *cross-document*
/// and their rules are uniformly worded, so the graph signals both
/// under-pair the real conflicts (a later decision often resolves with
/// no `attributed_to` entity, so entity-overlap can't reach it) and
/// over-pair claims that merely share a broad scope entity. They use
/// [`TensionStrategy::EmbeddingTopK`]: pair each claim with its nearest
/// neighbours by text embedding — a recall-oriented same-topic net that
/// hands the conflict-vs-compatible judgement to the LLM classifier.
/// (Embedding similarity tracks shared *topic*, not *conflict*: two
/// compatible rules on one topic are as similar as two conflicting ones,
/// so the selector cannot itself gate precision — that is the
/// classifier's job. Measured on the Maple House governance fixture.)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TensionStrategy {
    /// Cluster + entity-overlap + chunk-co-occurrence (the default).
    Graph,
    /// Top-K nearest claims by embedding cosine, floored at `floor`.
    EmbeddingTopK { k: usize, floor: f32 },
}

impl Default for TensionStrategy {
    fn default() -> Self {
        Self::Graph
    }
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
    /// Entity atoms in the resolved atlas. Required by the
    /// cross-position concept-overlap signal — the selector needs to
    /// know which `attributed_to` ids point at concept atoms (which
    /// in this domain stand in for "positions") and which point at
    /// person atoms ("proponents"). Defaults to empty in legacy call
    /// sites that haven't threaded entities yet; the cross-position
    /// signal degrades to no-op rather than panicking.
    #[allow(clippy::derivable_impls)]
    pub entities: &'a [Entity],
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
    out.extend(select_entity_overlap_claim_state(
        input.claims,
        input.states,
    ));
    out.extend(select_concept_overlap_cross_position(
        input.claims,
        input.entities,
    ));
    out.extend(select_chunk_cooccurrence_cross_position(
        input.claims,
        input.entities,
    ));
    // Same-named-speaker exclusion (see `drop_same_named_speaker_pairs`).
    drop_same_named_speaker_pairs(&mut out, input.claims, input.entities);
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

/// Drop tension candidate pairs whose two claims are attributed to the
/// SAME *named speaker* (a Person or Institution). A speaker's own claims
/// are one side of a debate, not two, so they cannot be in tension with
/// each other. This removes the dominant false-positive class on dense
/// corpora: an SEC proxy where the board authors dozens of mutually-
/// consistent claims, all near each other in embedding space and sharing
/// the "Board"/"directors" entities, otherwise yields hundreds of
/// board-vs-board candidate pairs the classifier must reject one by one.
///
/// CRUCIAL: `attributed_to` is overloaded across pipelines. In the
/// philosophy atlas it points at a *Concept* (a position); in governance
/// at a *topic* (also a Concept). There, two claims sharing an
/// `attributed_to` — a position contradicting itself, or a charter rule
/// vs a later decision on the same topic — ARE the real tensions. So this
/// excludes a same-`attributed_to` pair ONLY when the shared entity is a
/// Person/Institution (a named speaker). Concept/topic/unknown
/// attributions are left untouched, preserving philosophy + governance
/// recall. Applied strategy-agnostically (graph signals AND embedding
/// top-K) so the dense-corpus de-noising is uniform.
pub fn drop_same_named_speaker_pairs(
    candidates: &mut Vec<TensionCandidate>,
    claims: &[Claim],
    entities: &[Entity],
) {
    use crate::enrichment::pipeline::atlas::EntityType;
    let speaker: std::collections::HashMap<&AtomId, &AtomId> = claims
        .iter()
        .filter_map(|c| c.attributed_to.as_ref().map(|a| (&c.id, a)))
        .collect();
    let ent_type: std::collections::HashMap<&AtomId, &EntityType> =
        entities.iter().map(|e| (&e.id, &e.entity_type)).collect();
    candidates.retain(|cand| {
        if let (Some(a), Some(b)) =
            (speaker.get(&cand.source_atom), speaker.get(&cand.target_atom))
        {
            if a == b {
                let is_named_speaker = matches!(
                    ent_type.get(a),
                    Some(EntityType::Person) | Some(EntityType::Institution)
                );
                return !is_named_speaker;
            }
        }
        true
    });
}

/// Select tension candidates by embedding similarity — the
/// [`TensionStrategy::EmbeddingTopK`] strategy. For each claim, take its
/// `k` nearest *other* claims by cosine similarity, keeping only pairs at
/// or above `floor`, then dedup unordered pairs. `embeddings[i]` is the
/// vector for `claims[i]`; a length mismatch (or fewer than two claims,
/// or `k == 0`) yields no candidates.
///
/// Recall-oriented by design (see [`TensionStrategy`]): casts a
/// same-topic net wide enough to include the genuine conflicts and
/// leaves the conflict-vs-compatible decision to the classifier.
/// Deterministic: stable pair order, deduped by unordered atom-id key,
/// then `cand-NNNN` stamped — same inputs reproduce the same list.
pub fn select_embedding_topk(
    claims: &[Claim],
    embeddings: &[Vec<f32>],
    k: usize,
    floor: f32,
) -> Vec<TensionCandidate> {
    if claims.len() != embeddings.len() || claims.len() < 2 || k == 0 {
        return Vec::new();
    }
    // Per claim: its top-k neighbours at/above the floor. Collect both
    // directions (i→j and j→i); the dedup below collapses them to one
    // unordered pair, so a pair survives if it is a top-k neighbour of
    // *either* endpoint.
    let mut scored: Vec<(usize, usize, f32)> = Vec::new();
    for i in 0..claims.len() {
        let mut sims: Vec<(usize, f32)> = Vec::new();
        for (j, emb_j) in embeddings.iter().enumerate() {
            if i == j {
                continue;
            }
            let c = cosine_sim(&embeddings[i], emb_j);
            if c >= floor {
                sims.push((j, c));
            }
        }
        // Highest similarity first; index tie-break keeps it deterministic.
        sims.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        for (j, c) in sims.into_iter().take(k) {
            let (a, b) = if i < j { (i, j) } else { (j, i) };
            scored.push((a, b, c));
        }
    }
    // Deterministic emit order by (a, b); higher similarity wins the
    // dedup tie so the kept entry reflects the strongest signal.
    scored.sort_by(|x, y| {
        x.0.cmp(&y.0)
            .then(x.1.cmp(&y.1))
            .then(y.2.total_cmp(&x.2))
    });
    let mut seen: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let mut out: Vec<TensionCandidate> = Vec::new();
    for (a, b, _sim) in scored {
        if !seen.insert((a, b)) {
            continue;
        }
        out.push(TensionCandidate {
            id: String::new(),
            source_atom: claims[a].id.clone(),
            target_atom: claims[b].id.clone(),
            discovery: CandidateSource::EmbeddingTopK,
            cluster_id: None,
            shared_entity: None,
        });
    }
    for (i, c) in out.iter_mut().enumerate() {
        c.id = format!("cand-{:04}", i + 1);
    }
    tracing::debug!(
        target: "atlas.tensions",
        claims = claims.len(),
        k,
        floor,
        candidates = out.len(),
        "select_embedding_topk: candidate pairs"
    );
    out
}

/// Cosine similarity of two equal-length vectors. Returns 0.0 for
/// empty, length-mismatched, or zero-norm inputs (so a degenerate
/// embedding can never spuriously clear the floor).
fn cosine_sim(u: &[f32], v: &[f32]) -> f32 {
    if u.len() != v.len() || u.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut nu = 0.0f32;
    let mut nv = 0.0f32;
    for i in 0..u.len() {
        dot += u[i] * v[i];
        nu += u[i] * u[i];
        nv += v[i] * v[i];
    }
    if nu == 0.0 || nv == 0.0 {
        return 0.0;
    }
    dot / (nu.sqrt() * nv.sqrt())
}

fn select_intra_cluster(clusters: &[(String, Vec<AtomId>)]) -> Vec<TensionCandidate> {
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

fn select_entity_overlap_claim_state(claims: &[Claim], states: &[State]) -> Vec<TensionCandidate> {
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

/// Cross-position concept overlap. Pair every two claims attributed
/// to *different* concept entities (positions) when both claim
/// contents reference the *same* entity by canonical name.
///
/// Why this signal matters: the entity-overlap selectors above pair
/// atoms that share an `attributed_to` or `entity_id`, which by
/// construction surfaces *intra*-position tensions (two claims about
/// Pereboom's hard incompatibilism). Cross-position tensions —
/// compatibilism's view of moral responsibility vs hard
/// incompatibilism's view of moral responsibility — never appear in
/// that signal because the two claims have different attributions
/// and the deterministic enumerator doesn't see what concepts the
/// claim *content* engages.
///
/// The string-match heuristic is intentionally simple: scan each
/// claim's content for the canonical name of every entity in the
/// atlas; for each pair of claims with different position
/// attributions whose mention sets intersect, surface a candidate.
/// The shared entity becomes the candidate's `shared_entity`.
///
/// Filters:
/// - Only consider claims whose `attributed_to` resolves to an
///   `entity_type: concept` Entity. Person attributions (philosophers
///   as proponents) are skipped — a tension between "claims attributed
///   to Frankfurt" and "claims attributed to Pereboom" wouldn't
///   address the structural-position question, only a biographical
///   one.
/// - Only consider mentions of entities whose canonical name has at
///   least 4 characters AND is not the position itself. This filters
///   acronyms and self-mentions (a compatibilism claim that says
///   "compatibilism" doesn't tension itself).
/// - When multiple shared entities exist, pick the one with the
///   longest canonical name (a proxy for specificity). Stable for
///   reproducibility.
fn select_concept_overlap_cross_position(
    claims: &[Claim],
    entities: &[Entity],
) -> Vec<TensionCandidate> {
    if entities.is_empty() {
        // No entity context wired in — degrade to no-op so legacy
        // callers don't crash. The new signal is opt-in via the
        // `entities` field on `CandidateSelectionInput`.
        return Vec::new();
    }
    use std::collections::HashMap;
    let entity_by_id: HashMap<&AtomId, &Entity> = entities.iter().map(|e| (&e.id, e)).collect();

    // Pre-filter the entity name index for matchable mentions.
    // Names < 4 chars hit too often as substrings ("Mind", "X").
    // For each surviving entity, also pre-compute the token list:
    // contiguous alphabetic runs of length ≥ 5, then truncated to a
    // 5-char *prefix* used for substring matching. The prefix trick
    // catches morphological variants — claim text "morally
    // responsible" stem-matches the canonical "moral responsibility"
    // via token "respo" (prefix of "responsibility") found in
    // "responsible". Length-5 tokens are stop-word-y ("there",
    // "their"); we filter those by requiring ≥ 5 *alpha* chars AND
    // the token to appear in at least one position name (which is
    // discipline-specific by construction). Non-position concept
    // entities pass any 5-char token through.
    struct MatchableEntity<'a> {
        entity: &'a Entity,
        canonical_lower: String,
        tokens: Vec<String>,
    }
    // Stop-word-y short tokens that match too often. The list is
    // intentionally small; a longer stoplist would be a separate
    // concern. These five all appear as ≥5-char alpha runs in
    // common English prose and would over-fire if we stem-matched
    // them.
    const STOP_TOKENS: &[&str] = &[
        "there", "their", "these", "those", "where", "which", "would", "could", "about", "other",
        "after", "first", "every", "still", "ought",
    ];
    let is_stop = |t: &str| STOP_TOKENS.contains(&t);
    let matchable: Vec<MatchableEntity<'_>> = entities
        .iter()
        .filter(|e| e.canonical_name.trim().len() >= 4)
        .map(|e| {
            let canonical_lower = e.canonical_name.to_ascii_lowercase();
            // Take alpha-only runs ≥ 5 chars, prefix-truncated to 5
            // *characters* (not bytes — Russian patronymics in BK
            // arrive as mixed Latin/Cyrillic and a byte slice lands
            // mid-codepoint), dropping stop tokens.
            let tokens: Vec<String> = canonical_lower
                .split(|ch: char| !ch.is_alphabetic())
                .filter(|t| t.chars().count() >= 5)
                .map(|t| t.chars().take(5).collect::<String>())
                .filter(|t| !is_stop(t))
                .collect();
            MatchableEntity {
                entity: e,
                canonical_lower,
                tokens,
            }
        })
        .collect();

    // For each claim, resolve its attribution into a set of "position
    // concepts" (Concept entities the claim should pair against). A
    // claim attributed directly to a Concept resolves to that Concept.
    // A claim attributed to a Person resolves to any Concept whose
    // description mentions the Person's canonical name — this bridges
    // the common Phase 1 attribution pattern where the philosopher gets
    // the credit ("Aristotle holds…") rather than the position they
    // founded ("aristotelianism holds…"). Without this bridge, the
    // cross-position selector misses the dialectical core of texts
    // like virtue-ethics-fragments.
    let resolve_positions = |attr: &AtomId| -> Vec<&AtomId> {
        let Some(attr_entity) = entity_by_id.get(attr) else {
            return Vec::new();
        };
        match attr_entity.entity_type {
            EntityType::Concept => vec![&attr_entity.id],
            EntityType::Person => {
                // Match concept-description mentions on either the
                // full canonical name OR any individual name token of
                // length ≥ 4. Phase 1 typically writes "Foot's view"
                // or "MacIntyre argues..." inside concept descriptions
                // — full names like "Philippa Foot" wouldn't match,
                // but the surname token "foot" does.
                let canonical_lower = attr_entity.canonical_name.to_ascii_lowercase();
                let mut needles: Vec<String> = vec![canonical_lower.clone()];
                for token in canonical_lower
                    .split(|ch: char| !ch.is_alphabetic())
                    .filter(|t| t.len() >= 4)
                {
                    needles.push(token.to_string());
                }
                needles.retain(|n| !n.trim().is_empty());
                if needles.is_empty() {
                    return Vec::new();
                }
                entities
                    .iter()
                    .filter(|e| e.entity_type == EntityType::Concept)
                    .filter(|e| {
                        let desc = e.description.to_ascii_lowercase();
                        needles.iter().any(|n| desc.contains(n))
                    })
                    .map(|e| &e.id)
                    .collect()
            }
            _ => Vec::new(),
        }
    };

    struct ClaimMeta<'a> {
        claim: &'a Claim,
        positions: Vec<&'a AtomId>,
        mentions: std::collections::BTreeSet<&'a AtomId>,
    }
    let mut metas: Vec<ClaimMeta<'_>> = Vec::new();
    for c in claims {
        let Some(attr) = c.attributed_to.as_ref() else {
            continue;
        };
        let positions = resolve_positions(attr);
        if positions.is_empty() {
            continue;
        }
        let lower_content = c.content.to_ascii_lowercase();
        let mut mentions = std::collections::BTreeSet::new();
        for m in &matchable {
            // Two ways to match. Either:
            //   (a) the canonical name appears verbatim as a
            //       case-insensitive substring of the claim, OR
            //   (b) any of the canonical name's qualifying tokens
            //       (≥ 7 chars) appears as a substring.
            // Case (b) catches morphological variants and catches
            // claims that paraphrase the canonical phrase. The
            // self-position case is allowed (we want to surface
            // "claim attributed to A mentions A's name in context
            // of position B").
            let canonical_hit = lower_content.contains(&m.canonical_lower);
            let token_hit = m.tokens.iter().any(|t| lower_content.contains(t.as_str()));
            if canonical_hit || token_hit {
                mentions.insert(&m.entity.id);
            }
        }
        if mentions.is_empty() {
            continue;
        }
        metas.push(ClaimMeta {
            claim: c,
            positions,
            mentions,
        });
    }

    // Pair every (i, j) where the two claims' position sets are
    // disjoint AND mention sets intersect. A position is "shared" if
    // it appears in both claims' resolved-positions list — those are
    // intra-position pairs already covered by the entity-overlap
    // selector and we skip them here. Stable order: outer loop
    // preserves claim order from input.
    let mut out = Vec::new();
    let mut emitted_pairs: std::collections::HashSet<(usize, usize)> =
        std::collections::HashSet::new();
    for i in 0..metas.len() {
        for j in (i + 1)..metas.len() {
            let pos_i: std::collections::BTreeSet<&AtomId> =
                metas[i].positions.iter().copied().collect();
            let pos_j: std::collections::BTreeSet<&AtomId> =
                metas[j].positions.iter().copied().collect();
            if !pos_i.is_disjoint(&pos_j) {
                continue;
            }
            if !emitted_pairs.insert((i, j)) {
                continue;
            }
            // Intersection: pick the shared entity with the longest
            // canonical name as the candidate's `shared_entity`.
            // Skip mentions that are *either claim's own position* —
            // those carry less interpretive weight than a third
            // shared concept. We tolerate them as fallbacks if no
            // third concept is shared.
            let mut best_shared: Option<&AtomId> = None;
            let mut best_len = 0usize;
            let mut fallback_shared: Option<&AtomId> = None;
            let mut fallback_len = 0usize;
            for id in metas[i].mentions.intersection(&metas[j].mentions) {
                let Some(e) = entity_by_id.get(id) else {
                    continue;
                };
                let is_own_position = pos_i.contains(id) || pos_j.contains(id);
                let len = e.canonical_name.len();
                if is_own_position {
                    if len > fallback_len {
                        fallback_len = len;
                        fallback_shared = Some(*id);
                    }
                } else if len > best_len {
                    best_len = len;
                    best_shared = Some(*id);
                }
            }
            let Some(shared) = best_shared.or(fallback_shared) else {
                continue;
            };
            out.push(TensionCandidate {
                id: String::new(),
                source_atom: metas[i].claim.id.clone(),
                target_atom: metas[j].claim.id.clone(),
                discovery: CandidateSource::EntityOverlap,
                cluster_id: None,
                shared_entity: Some(shared.clone()),
            });
        }
    }
    out
}

/// Resolve a claim's `attributed_to` atom id into the set of "position
/// concepts" it represents. A Concept attribution resolves to itself.
/// A Person attribution bridges to any Concept whose description
/// mentions the person — full canonical name OR any name token of
/// length ≥ 4 (so "Foot" in a Concept's description bridges
/// "Philippa Foot" attributions). Anything else returns empty.
fn resolve_positions_for_attribution<'a>(
    attr: &AtomId,
    entities: &'a [Entity],
    entity_by_id: &std::collections::HashMap<&'a AtomId, &'a Entity>,
) -> Vec<&'a AtomId> {
    let Some(attr_entity) = entity_by_id.get(attr) else {
        return Vec::new();
    };
    match attr_entity.entity_type {
        EntityType::Concept => vec![&attr_entity.id],
        EntityType::Person => {
            let canonical_lower = attr_entity.canonical_name.to_ascii_lowercase();
            let mut needles: Vec<String> = vec![canonical_lower.clone()];
            for token in canonical_lower
                .split(|ch: char| !ch.is_alphabetic())
                .filter(|t| t.len() >= 4)
            {
                needles.push(token.to_string());
            }
            needles.retain(|n| !n.trim().is_empty());
            if needles.is_empty() {
                return Vec::new();
            }
            entities
                .iter()
                .filter(|e| e.entity_type == EntityType::Concept)
                .filter(|e| {
                    let desc = e.description.to_ascii_lowercase();
                    needles.iter().any(|n| desc.contains(n))
                })
                .map(|e| &e.id)
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Chunk co-occurrence cross-position signal.
///
/// Pair every two claims attributed to *different* positions whose
/// evidence lands in at least one shared Phase-1 section. The
/// rationale: two positions discussed in the same section are by
/// construction in dialectical conversation. Where the
/// `concept-overlap` selector requires both claims to mention a
/// shared third concept by name, this selector only requires
/// section-level co-occurrence — catching structural disagreements
/// the model phrases without naming a common anchor (Stoics talk
/// about virtue, Epicureans talk about pleasure; both in the same
/// chapter on Hellenistic ethics).
///
/// Filter discipline (matches `select_concept_overlap_cross_position`):
/// - Both claims must be attributed.
/// - Each attribution resolves through `resolve_positions_for_attribution`
///   into a non-empty set of position Concepts (Person attributions
///   bridge via concept descriptions).
/// - The two claims' position sets must be *disjoint* — if any
///   position is shared, the claims describe the same school's
///   commitments and aren't a cross-position candidate.
///
/// This signal will overlap with `concept-overlap`. The dedup pass
/// downstream prefers `EntityOverlap` over `ChunkCooccurrence`, so
/// a pair surfaced by both keeps the more specific provenance.
fn select_chunk_cooccurrence_cross_position(
    claims: &[Claim],
    entities: &[Entity],
) -> Vec<TensionCandidate> {
    if entities.is_empty() {
        return Vec::new();
    }
    use std::collections::{BTreeSet, HashMap};
    let entity_by_id: HashMap<&AtomId, &Entity> = entities.iter().map(|e| (&e.id, e)).collect();

    struct ClaimMeta<'a> {
        claim: &'a Claim,
        positions: Vec<&'a AtomId>,
        sections: BTreeSet<&'a str>,
    }
    let metas: Vec<ClaimMeta<'_>> = claims
        .iter()
        .filter_map(|c| {
            let attr = c.attributed_to.as_ref()?;
            let positions = resolve_positions_for_attribution(attr, entities, &entity_by_id);
            if positions.is_empty() {
                return None;
            }
            let sections: BTreeSet<&str> = c.evidence.iter().map(|e| e.chunk_id.as_str()).collect();
            if sections.is_empty() {
                return None;
            }
            Some(ClaimMeta {
                claim: c,
                positions,
                sections,
            })
        })
        .collect();

    let mut out = Vec::new();
    for i in 0..metas.len() {
        for j in (i + 1)..metas.len() {
            let pi: BTreeSet<&AtomId> = metas[i].positions.iter().copied().collect();
            let pj: BTreeSet<&AtomId> = metas[j].positions.iter().copied().collect();
            if !pi.is_disjoint(&pj) {
                continue;
            }
            if metas[i].sections.is_disjoint(&metas[j].sections) {
                continue;
            }
            out.push(TensionCandidate {
                id: String::new(),
                source_atom: metas[i].claim.id.clone(),
                target_atom: metas[j].claim.id.clone(),
                discovery: CandidateSource::ChunkCooccurrence,
                cluster_id: None,
                shared_entity: None,
            });
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
        CandidateSource::ChunkCooccurrence => 2,
        CandidateSource::EmbeddingTopK => 3,
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
    use super::super::super::atoms::{ChunkRef, SectionRange};
    use super::*;
    use crate::enrichment::pipeline::atlas::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EpistemicStatus, StateType,
    };

    fn claim(id: u32, attributed_to: Option<u32>) -> Claim {
        Claim {
            id: AtomId::from_raw(format!("claim-{id:04}")),
            content: format!("claim {id}"),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: vec![ChunkRef::new("sec_0001", None)],
            attributed_to: attributed_to.map(|e| AtomId::from_raw(format!("entity-{e:04}"))),
            confidence: Some(1.0),
            anchor: None,
            enrichment_depth: EnrichmentDepth::Extracted,
            quotable_excerpt: None,
            claim_kind: None,
            concession_outcome: None,
            evidence_kind: None,
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

    fn unordered(out: &[TensionCandidate]) -> std::collections::HashSet<(String, String)> {
        out.iter()
            .map(|c| {
                let a = c.source_atom.as_str().to_string();
                let b = c.target_atom.as_str().to_string();
                if a < b { (a, b) } else { (b, a) }
            })
            .collect()
    }

    #[test]
    fn same_named_speaker_claims_are_not_paired() {
        use crate::enrichment::pipeline::atlas::EntityType;
        // Two claims authored by the SAME Institution (the board), sharing
        // that attributed_to entity. Entity-overlap would pair them, but a
        // speaker's own claims are one side, not two — the same-speaker
        // filter must drop the pair.
        let entities = vec![entity(1, "Board of Directors", EntityType::Institution)];
        let out = select_candidates(CandidateSelectionInput {
            claims: &[claim(1, Some(1)), claim(2, Some(1))],
            states: &[],
            claim_clusters: &[],
            entities: &entities,
        });
        assert!(
            out.is_empty(),
            "same-Institution-speaker claims must not be tension candidates, got {out:?}"
        );
    }

    #[test]
    fn same_concept_position_claims_still_pair() {
        use crate::enrichment::pipeline::atlas::EntityType;
        // attributed_to here is a Concept (a philosophy position / a
        // governance topic). Two conflicting claims under it ARE a real
        // tension, so the same-speaker filter must NOT drop them.
        let entities = vec![entity(1, "SharedPosition", EntityType::Concept)];
        let mut a = claim(1, Some(1));
        a.content = "SharedPosition argues the thesis holds.".into();
        let mut b = claim(2, Some(1));
        b.content = "SharedPosition denies the thesis holds.".into();
        let out = select_candidates(CandidateSelectionInput {
            claims: &[a, b],
            states: &[],
            claim_clusters: &[],
            entities: &entities,
        });
        assert!(
            !out.is_empty(),
            "same-Concept-position conflicting claims must remain candidates"
        );
    }

    #[test]
    fn embedding_topk_pairs_nearest_neighbours_not_orthogonal() {
        // Two topics: {0,1} similar, {2,3} similar, orthogonal across.
        let claims = vec![claim(0, None), claim(1, None), claim(2, None), claim(3, None)];
        let embs = vec![
            vec![1.0, 0.0],
            vec![0.96, 0.28], // ~cos 0.96 with claim 0
            vec![0.0, 1.0],
            vec![0.28, 0.96], // ~cos 0.96 with claim 2
        ];
        let out = select_embedding_topk(&claims, &embs, 1, 0.5);
        let pairs = unordered(&out);
        assert!(pairs.contains(&("claim-0000".into(), "claim-0001".into())));
        assert!(pairs.contains(&("claim-0002".into(), "claim-0003".into())));
        // Orthogonal cross-topic pairs are below the floor — not emitted.
        assert!(!pairs.contains(&("claim-0000".into(), "claim-0002".into())));
        assert!(out
            .iter()
            .all(|c| matches!(c.discovery, CandidateSource::EmbeddingTopK)));
    }

    #[test]
    fn embedding_topk_floor_filters_weak_pairs() {
        let claims = vec![claim(0, None), claim(1, None)];
        let embs = vec![vec![1.0, 0.0], vec![0.0, 1.0]]; // cosine 0
        assert!(select_embedding_topk(&claims, &embs, 5, 0.5).is_empty());
    }

    #[test]
    fn embedding_topk_dedups_symmetric_pairs() {
        let claims = vec![claim(0, None), claim(1, None)];
        let embs = vec![vec![1.0, 0.0], vec![0.99, 0.14]];
        // Each is the other's nearest neighbour; one undirected pair emitted.
        let out = select_embedding_topk(&claims, &embs, 5, 0.5);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "cand-0001");
    }

    #[test]
    fn embedding_topk_length_mismatch_or_too_few_is_empty() {
        let claims = vec![claim(0, None), claim(1, None)];
        assert!(select_embedding_topk(&claims, &[vec![1.0, 0.0]], 5, 0.5).is_empty());
        assert!(select_embedding_topk(&[claim(0, None)], &[vec![1.0]], 5, 0.5).is_empty());
        assert!(select_embedding_topk(&claims, &[vec![1.0], vec![1.0]], 0, 0.5).is_empty());
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
            entities: &[],
        });
        // 3 claims → C(3,2) = 3 unique pairs.
        assert_eq!(out.len(), 3);
        assert!(out
            .iter()
            .all(|c| matches!(c.discovery, CandidateSource::IntraCluster)));
        assert!(out
            .iter()
            .all(|c| c.cluster_id.as_deref() == Some("cluster-01")));
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
            entities: &[],
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
            entities: &[],
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
            entities: &[],
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
            entities: &[],
        });
        let ids: Vec<&str> = out.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["cand-0001", "cand-0002", "cand-0003"]);
    }

    fn entity(id: u32, name: &str, kind: crate::enrichment::pipeline::atlas::EntityType) -> Entity {
        Entity {
            id: AtomId::from_raw(format!("entity-{id:04}")),
            canonical_name: name.into(),
            aliases: Vec::new(),
            entity_type: kind,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: format!("test entity {id}"),
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            defining_quote: None,
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        }
    }

    /// Two claims attributed to two different concept-typed entities,
    /// whose contents both mention the canonical name of a third
    /// entity (the shared concept). The cross-position selector must
    /// pair them and tag the shared entity.
    #[test]
    fn cross_position_claims_sharing_a_concept_pair() {
        use crate::enrichment::pipeline::atlas::EntityType;
        let entities = vec![
            entity(1, "PositionAlpha", EntityType::Concept),
            entity(2, "PositionBeta", EntityType::Concept),
            entity(3, "thirdConcept", EntityType::Concept),
        ];
        let mut a = claim(1, Some(1));
        a.content = "PositionAlpha holds that thirdConcept is grounded in causal history.".into();
        let mut b = claim(2, Some(2));
        b.content = "PositionBeta denies that thirdConcept is grounded; it floats free.".into();
        let out = select_candidates(CandidateSelectionInput {
            claims: &[a, b],
            states: &[],
            claim_clusters: &[],
            entities: &entities,
        });
        // Should produce at least one cross-position candidate. The
        // intra-position selectors don't fire because positions
        // differ; the cross-position selector should fire on the
        // shared mention of "thirdConcept".
        assert_eq!(
            out.len(),
            1,
            "expected exactly one cross-position candidate, got {out:?}"
        );
        let c = &out[0];
        assert!(matches!(c.discovery, CandidateSource::EntityOverlap));
        assert_eq!(
            c.shared_entity.as_ref().map(|a| a.as_str()),
            Some("entity-0003"),
            "shared entity should be the third (longest-name) concept"
        );
    }

    /// Claims attributed to the SAME position should not produce
    /// cross-position candidates. The intra-position selector
    /// already covers them.
    #[test]
    fn cross_position_same_position_does_not_pair() {
        use crate::enrichment::pipeline::atlas::EntityType;
        let entities = vec![
            entity(1, "SharedPosition", EntityType::Concept),
            entity(3, "thirdConcept", EntityType::Concept),
        ];
        let mut a = claim(1, Some(1));
        a.content = "SharedPosition argues thirdConcept matters.".into();
        let mut b = claim(2, Some(1));
        b.content = "SharedPosition denies thirdConcept matters.".into();
        let out = select_candidates(CandidateSelectionInput {
            claims: &[a, b],
            states: &[],
            claim_clusters: &[],
            entities: &entities,
        });
        // Intra-position pairing fires (same `attributed_to`); the
        // cross-position selector must NOT also fire.
        let cross_count = out
            .iter()
            .filter(|c| c.shared_entity.as_ref().map(|s| s.as_str()) == Some("entity-0003"))
            .count();
        assert_eq!(
            cross_count, 0,
            "cross-position selector should not pair same-position claims, got {out:?}"
        );
    }

    /// Claims attributed to person entities should NOT yield
    /// cross-position candidates when no concept entity bridges the
    /// philosophers — i.e. when neither person appears in any
    /// concept's description, the bridge has nothing to lean on.
    #[test]
    fn cross_position_skips_person_attributions_without_bridge() {
        use crate::enrichment::pipeline::atlas::EntityType;
        let entities = vec![
            entity(1, "Aquinas", EntityType::Person),
            entity(2, "Spinoza", EntityType::Person),
            entity(3, "thirdConcept", EntityType::Concept),
        ];
        let mut a = claim(1, Some(1));
        a.content = "Aquinas argues thirdConcept matters.".into();
        let mut b = claim(2, Some(2));
        b.content = "Spinoza denies thirdConcept matters.".into();
        let out = select_candidates(CandidateSelectionInput {
            claims: &[a, b],
            states: &[],
            claim_clusters: &[],
            entities: &entities,
        });
        assert!(
            out.is_empty(),
            "person attributions with no concept-description bridge should not yield candidates, got {out:?}"
        );
    }

    /// Person→Position bridge: a claim attributed to a Person whose
    /// name appears in a Concept's description resolves to that
    /// Concept for cross-position purposes. Pairs the claim against
    /// claims attributed to *other* concepts when their content
    /// shares a mention.
    #[test]
    fn cross_position_bridges_person_via_concept_description() {
        use crate::enrichment::pipeline::atlas::EntityType;
        let mut aristotelianism = entity(1, "aristotelianism", EntityType::Concept);
        aristotelianism.description =
            "The position founded by Aristotle, centered on virtue and eudaimonia.".into();
        let mut situationism = entity(2, "situationism", EntityType::Concept);
        situationism.description =
            "A skeptical position about character, advanced by Doris and Harman.".into();
        let entities = vec![
            aristotelianism,
            situationism,
            entity(3, "Aristotle", EntityType::Person),
            entity(4, "virtue", EntityType::Concept),
        ];
        let mut a = claim(1, Some(3));
        a.content = "Aristotle argues that virtue is the foundation of flourishing.".into();
        let mut b = claim(2, Some(2));
        b.content = "Situationism denies that virtue is a stable trait.".into();
        let out = select_candidates(CandidateSelectionInput {
            claims: &[a, b],
            states: &[],
            claim_clusters: &[],
            entities: &entities,
        });
        let cross: Vec<_> = out
            .iter()
            .filter(|c| matches!(c.discovery, CandidateSource::EntityOverlap))
            .filter(|c| {
                c.source_atom.as_str() == "claim-0001" && c.target_atom.as_str() == "claim-0002"
            })
            .collect();
        assert_eq!(
            cross.len(),
            1,
            "expected one cross-position candidate via Person→Concept bridge, got {out:?}"
        );
    }

    /// Bridge via surname: when the Person canonical_name is full
    /// ("Philippa Foot") but the concept description references only
    /// the surname ("Foot's view"), the bridge must still fire.
    #[test]
    fn cross_position_bridges_person_via_surname_token() {
        use crate::enrichment::pipeline::atlas::EntityType;
        let mut realism = entity(1, "metaphysical realism", EntityType::Concept);
        realism.description =
            "The label for Foot's neo-Aristotelism, holding objective virtue facts.".into();
        let mut sentimentalism = entity(2, "sentimentalism", EntityType::Concept);
        sentimentalism.description =
            "A branch of virtue ethics holding virtues are sentiment-based traits.".into();
        let entities = vec![
            realism,
            sentimentalism,
            entity(3, "Philippa Foot", EntityType::Person),
            entity(4, "virtue", EntityType::Concept),
        ];
        let mut a = claim(1, Some(3));
        a.content = "Philippa Foot grounds virtue in natural human function.".into();
        let mut b = claim(2, Some(2));
        b.content = "Sentimentalism takes virtue as a sentiment-driven trait.".into();
        let out = select_candidates(CandidateSelectionInput {
            claims: &[a, b],
            states: &[],
            claim_clusters: &[],
            entities: &entities,
        });
        let cross: Vec<_> = out
            .iter()
            .filter(|c| {
                c.source_atom.as_str() == "claim-0001" && c.target_atom.as_str() == "claim-0002"
            })
            .collect();
        assert_eq!(
            cross.len(),
            1,
            "surname-token bridge should fire when concept desc references 'Foot'"
        );
    }

    /// Two claims attributed to different concept-typed positions whose
    /// evidence lands in the same section get a chunk-co-occurrence
    /// candidate even when their content shares no entity mentions.
    /// This is the core motivating case from stoic-test (Stoics talk
    /// about virtue, Epicureans talk about pleasure — same chapter,
    /// no shared anchor in content).
    #[test]
    fn chunk_cooccurrence_pairs_cross_position_claims_in_same_section() {
        use crate::enrichment::pipeline::atlas::EntityType;
        let entities = vec![
            entity(1, "Stoicism", EntityType::Concept),
            entity(2, "Epicureans", EntityType::Concept),
        ];
        let mut a = claim(1, Some(1));
        a.content = "Only virtue is good.".into();
        a.evidence = vec![ChunkRef::new("sec_0004", None)];
        let mut b = claim(2, Some(2));
        b.content = "Pleasure is the absence of bodily pain.".into();
        b.evidence = vec![ChunkRef::new("sec_0004", None)];
        let out = select_candidates(CandidateSelectionInput {
            claims: &[a, b],
            states: &[],
            claim_clusters: &[],
            entities: &entities,
        });
        let cooc: Vec<_> = out
            .iter()
            .filter(|c| matches!(c.discovery, CandidateSource::ChunkCooccurrence))
            .collect();
        assert_eq!(
            cooc.len(),
            1,
            "expected one chunk-cooccurrence candidate, got {out:?}"
        );
    }

    /// Same section, same position → no candidate. The selector
    /// targets cross-position structural disagreement, not
    /// intra-position elaboration.
    #[test]
    fn chunk_cooccurrence_skips_same_position_pairs() {
        use crate::enrichment::pipeline::atlas::EntityType;
        let entities = vec![entity(1, "Stoicism", EntityType::Concept)];
        let mut a = claim(1, Some(1));
        a.evidence = vec![ChunkRef::new("sec_0001", None)];
        let mut b = claim(2, Some(1));
        b.evidence = vec![ChunkRef::new("sec_0001", None)];
        let out = select_candidates(CandidateSelectionInput {
            claims: &[a, b],
            states: &[],
            claim_clusters: &[],
            entities: &entities,
        });
        let cooc: Vec<_> = out
            .iter()
            .filter(|c| matches!(c.discovery, CandidateSource::ChunkCooccurrence))
            .collect();
        assert!(
            cooc.is_empty(),
            "same-position pairs should not produce chunk-cooccurrence candidates"
        );
    }

    /// Different positions in different sections → no candidate.
    /// The dialectical signal is co-occurrence; if the corpus didn't
    /// place them in the same section, we don't pair them here.
    #[test]
    fn chunk_cooccurrence_skips_disjoint_sections() {
        use crate::enrichment::pipeline::atlas::EntityType;
        let entities = vec![
            entity(1, "Stoicism", EntityType::Concept),
            entity(2, "Epicureans", EntityType::Concept),
        ];
        let mut a = claim(1, Some(1));
        a.evidence = vec![ChunkRef::new("sec_0001", None)];
        let mut b = claim(2, Some(2));
        b.evidence = vec![ChunkRef::new("sec_0007", None)];
        let out = select_candidates(CandidateSelectionInput {
            claims: &[a, b],
            states: &[],
            claim_clusters: &[],
            entities: &entities,
        });
        let cooc: Vec<_> = out
            .iter()
            .filter(|c| matches!(c.discovery, CandidateSource::ChunkCooccurrence))
            .collect();
        assert!(cooc.is_empty(), "disjoint sections should not pair");
    }

    /// Person→Concept bridge applies here too. A Person attribution
    /// whose surname appears in a Concept's description bridges to
    /// that Concept, allowing chunk-cooccurrence pairing across
    /// philosopher-attributed and position-attributed claims in the
    /// same section.
    #[test]
    fn chunk_cooccurrence_bridges_person_via_concept_description() {
        use crate::enrichment::pipeline::atlas::EntityType;
        let mut realism = entity(1, "metaphysical realism", EntityType::Concept);
        realism.description =
            "The label for Foot's neo-Aristotelism, holding objective virtue facts.".into();
        let entities = vec![
            realism,
            entity(2, "situationism", EntityType::Concept),
            entity(3, "Philippa Foot", EntityType::Person),
        ];
        let mut a = claim(1, Some(3));
        a.evidence = vec![ChunkRef::new("sec_0002", None)];
        let mut b = claim(2, Some(2));
        b.evidence = vec![ChunkRef::new("sec_0002", None)];
        let out = select_candidates(CandidateSelectionInput {
            claims: &[a, b],
            states: &[],
            claim_clusters: &[],
            entities: &entities,
        });
        let cooc: Vec<_> = out
            .iter()
            .filter(|c| matches!(c.discovery, CandidateSource::ChunkCooccurrence))
            .collect();
        assert_eq!(
            cooc.len(),
            1,
            "Person→Concept bridge should propagate into chunk-cooccurrence selector"
        );
    }

    /// Dedup: when the same pair fires both EntityOverlap and
    /// ChunkCooccurrence, the dedup pass keeps the EntityOverlap
    /// provenance (more specific signal). The pair appears once in
    /// the final list.
    #[test]
    fn chunk_cooccurrence_yields_to_entity_overlap_in_dedup() {
        use crate::enrichment::pipeline::atlas::EntityType;
        let entities = vec![
            entity(1, "PositionAlpha", EntityType::Concept),
            entity(2, "PositionBeta", EntityType::Concept),
            entity(3, "thirdConcept", EntityType::Concept),
        ];
        let mut a = claim(1, Some(1));
        a.content = "PositionAlpha argues thirdConcept matters.".into();
        a.evidence = vec![ChunkRef::new("sec_0001", None)];
        let mut b = claim(2, Some(2));
        b.content = "PositionBeta denies thirdConcept matters.".into();
        b.evidence = vec![ChunkRef::new("sec_0001", None)];
        let out = select_candidates(CandidateSelectionInput {
            claims: &[a, b],
            states: &[],
            claim_clusters: &[],
            entities: &entities,
        });
        // Exactly one candidate for this pair, EntityOverlap wins.
        let pair_cands: Vec<_> = out
            .iter()
            .filter(|c| {
                c.source_atom.as_str() == "claim-0001" && c.target_atom.as_str() == "claim-0002"
            })
            .collect();
        assert_eq!(pair_cands.len(), 1);
        assert!(matches!(
            pair_cands[0].discovery,
            CandidateSource::EntityOverlap
        ));
    }
}
