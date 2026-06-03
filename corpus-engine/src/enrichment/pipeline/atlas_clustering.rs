//! Atlas facet-typed clustering — Phase 2 of the v2.1 pipeline.
//!
//! Wraps `vector_clustering::cluster_vectors` for each atlas facet.
//! The HDBSCAN primary pass is facet-agnostic; facet-specific
//! secondary signals (attributed-entity overlap, participant
//! overlap, temporal proximity) apply as a post-pass re-labelling
//! step, so the core math stays one code path.
//!
//! Secondary signals intentionally only *merge* small near-by
//! clusters that share a facet-specific field (e.g. two claim
//! clusters both attributed to "Zosima" with ≥ 0.55 centroid
//! cosine). They never split a cluster HDBSCAN produced. The
//! intuition: HDBSCAN has full embedding visibility and lands
//! correct decisions at the coarse granularity; post-pass merges
//! bridge cases where an entity overlap pulls two semantically
//! adjacent clusters together without us second-guessing HDBSCAN's
//! density calls.

use std::collections::{HashMap, HashSet};

use tracing::debug;

use crate::enrichment::domain::ClusteringConfig;
use crate::enrichment::pipeline::atlas::{
    ClaimSketch, EntityStateSketch, EventSketch, QuestionSketch, RelationStateSketch,
    SectionExtraction,
};
use crate::enrichment::pipeline::types::{AtlasCluster, Facet, SketchRef};
use crate::enrichment::pipeline::vector_clustering::cluster_vectors;
use crate::error::Result;
use crate::types::EmbedFn;

// ── Tuning constants ────────────────────────────────────────

/// Cosine floor for the post-HDBSCAN merge pass. Two clusters with
/// a shared secondary-signal field (attributed-entity, participant
/// set, section proximity) merge iff their centroid cosine is at
/// least this. 0.55 is deliberately permissive — HDBSCAN wouldn't
/// have separated them in the first place if their centroids were
/// close; the merge is meant to reunite near-neighbours that
/// density split, not to fuse unrelated clusters.
pub const SECONDARY_SIGNAL_MERGE_COSINE: f32 = 0.55;

/// Maximum section distance for the temporal-proximity signal on
/// entity/relation state clusters. A state cluster that spans 3
/// sections merges with a neighbour whose earliest section is
/// within this many sections of its latest.
pub const STATE_TEMPORAL_PROXIMITY_SECTIONS: usize = 3;

// ── Public API ──────────────────────────────────────────────

/// Output of clustering a single facet. The runner builds one of
/// these per `Facet`, then folds them into a single
/// `Phase2AtlasOutput`.
#[derive(Debug, Clone)]
pub struct FacetClusterResult {
    pub facet: Facet,
    pub clusters: Vec<AtlasCluster>,
    pub unclustered: Vec<SketchRef>,
}

/// Cluster every facet inside `sections` using the appropriate
/// secondary-signal post-pass. Returns one `FacetClusterResult`
/// per facet that had at least one sketch.
///
/// Embeddings are computed by the caller-supplied `embed_fn` —
/// the same function the runner uses everywhere else. Keeps the
/// cost path transparent: we don't introduce a new embedding
/// model or batch shape at this layer.
pub async fn cluster_all_facets(
    sections: &[SectionExtraction],
    embed_fn: &EmbedFn,
    config: &ClusteringConfig,
) -> Result<Vec<FacetClusterResult>> {
    let mut out = Vec::with_capacity(Facet::ALL.len());
    for &facet in Facet::ALL {
        let result = cluster_facet(sections, facet, embed_fn, config).await?;
        if !result.clusters.is_empty() || !result.unclustered.is_empty() {
            out.push(result);
        }
    }
    Ok(out)
}

/// Cluster a single facet. Exposed for tests and for callers that
/// want to re-cluster just one facet (e.g. after tuning its
/// exemplar bank).
pub async fn cluster_facet(
    sections: &[SectionExtraction],
    facet: Facet,
    embed_fn: &EmbedFn,
    config: &ClusteringConfig,
) -> Result<FacetClusterResult> {
    // 1. Collect sketches for this facet with their section+index
    //    provenance, plus the per-sketch embedding seed text.
    let sketches = gather_sketches(sections, facet);
    if sketches.is_empty() {
        return Ok(FacetClusterResult {
            facet,
            clusters: Vec::new(),
            unclustered: Vec::new(),
        });
    }

    // 2. Embed every seed text. One embed call per sketch, same
    //    as the runner's usual rhythm.
    let mut embeddings: Vec<Vec<f32>> = Vec::with_capacity(sketches.len());
    for sk in &sketches {
        embeddings.push((embed_fn)(&sk.embed_text).await?);
    }

    // 3. Run HDBSCAN.
    let raw = cluster_vectors(&embeddings, config)?;
    debug!(
        facet = facet.as_str(),
        sketches = sketches.len(),
        clusters = raw.cluster_count,
        noise = raw.noise_count,
        "atlas_clustering: primary pass"
    );

    // 4. Build initial cluster membership + noise list.
    let members_by_cluster = raw.members_by_cluster();
    let mut clusters: Vec<ClusterDraft> = members_by_cluster
        .into_iter()
        .map(|(label, member_indices)| {
            let centroid = centroid_of(&member_indices, &embeddings);
            ClusterDraft {
                label,
                member_indices,
                centroid,
                secondary_signal: Vec::new(),
            }
        })
        .collect();
    // Sort by HDBSCAN label so ids are deterministic across runs.
    clusters.sort_by_key(|c| c.label);

    // 5. Populate per-cluster secondary-signal features. Which
    //    signal applies depends on the facet.
    for cluster in &mut clusters {
        cluster.secondary_signal =
            collect_secondary_signal(facet, &cluster.member_indices, &sketches);
    }

    // 6. Merge pass. Pairwise over existing clusters; merges are
    //    recorded and applied in a second pass so iteration is
    //    stable.
    let merges = find_secondary_signal_merges(&clusters);
    let clusters_after_merge = apply_merges(clusters, merges, &embeddings);

    // 7. Assemble the public shape.
    let mut out_clusters = Vec::with_capacity(clusters_after_merge.len());
    for (ix, draft) in clusters_after_merge.iter().enumerate() {
        let refs = draft
            .member_indices
            .iter()
            .map(|&i| SketchRef {
                section_id: sketches[i].section_id.clone(),
                facet,
                sketch_index: sketches[i].sketch_index,
            })
            .collect();
        out_clusters.push(AtlasCluster {
            id: format!("{}_cl_{ix:04}", facet.as_str()),
            facet,
            refs,
        });
    }
    let unclustered: Vec<SketchRef> = raw
        .labels
        .iter()
        .enumerate()
        .filter(|(_, &label)| label < 0)
        .map(|(i, _)| SketchRef {
            section_id: sketches[i].section_id.clone(),
            facet,
            sketch_index: sketches[i].sketch_index,
        })
        .collect();

    Ok(FacetClusterResult {
        facet,
        clusters: out_clusters,
        unclustered,
    })
}

// ── Sketch gathering ────────────────────────────────────────

/// Pulls sketches of a single facet out of the section list into a
/// flat, index-stable list. Carries enough metadata for the
/// secondary-signal pass to reason about attributed-entity,
/// participant overlap, and section ordinals.
struct SketchHandle {
    section_id: String,
    /// Ordinal position in `sections` — used for temporal-proximity
    /// merges on state clusters.
    section_ordinal: usize,
    sketch_index: usize,
    /// The text fed to the embed model. Different per facet to
    /// surface the feature the clusterer should cluster on.
    embed_text: String,
    /// Case-folded attributed_to (for claims) or joined participants
    /// (for relation-states + events) used by the secondary signal.
    /// `None` means "no secondary feature available for this sketch."
    secondary_key: Option<String>,
}

fn gather_sketches(sections: &[SectionExtraction], facet: Facet) -> Vec<SketchHandle> {
    let mut out = Vec::new();
    for (ordinal, section) in sections.iter().enumerate() {
        match facet {
            Facet::Question => {
                for (i, q) in section.questions_raised.iter().enumerate() {
                    out.push(from_question(section, ordinal, i, q));
                }
            }
            Facet::Claim => {
                for (i, c) in section.claims.iter().enumerate() {
                    out.push(from_claim(section, ordinal, i, c));
                }
            }
            Facet::EntityState => {
                for (i, s) in section.entities_developed.iter().enumerate() {
                    out.push(from_entity_state(section, ordinal, i, s));
                }
            }
            Facet::RelationState => {
                for (i, s) in section.relations_developed.iter().enumerate() {
                    out.push(from_relation_state(section, ordinal, i, s));
                }
            }
            Facet::Event => {
                for (i, e) in section.events.iter().enumerate() {
                    out.push(from_event(section, ordinal, i, e));
                }
            }
        }
    }
    out
}

fn from_question(
    section: &SectionExtraction,
    ordinal: usize,
    index: usize,
    q: &QuestionSketch,
) -> SketchHandle {
    SketchHandle {
        section_id: section.section_id.clone(),
        section_ordinal: ordinal,
        sketch_index: index,
        embed_text: q.content.clone(),
        secondary_key: None,
    }
}

fn from_claim(
    section: &SectionExtraction,
    ordinal: usize,
    index: usize,
    c: &ClaimSketch,
) -> SketchHandle {
    SketchHandle {
        section_id: section.section_id.clone(),
        section_ordinal: ordinal,
        sketch_index: index,
        embed_text: c.content.clone(),
        secondary_key: c
            .attributed_to
            .as_ref()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty()),
    }
}

fn from_entity_state(
    section: &SectionExtraction,
    ordinal: usize,
    index: usize,
    s: &EntityStateSketch,
) -> SketchHandle {
    SketchHandle {
        section_id: section.section_id.clone(),
        section_ordinal: ordinal,
        sketch_index: index,
        // Seed the embedder with both the entity name and the
        // state label so the cluster topology honours *which
        // character* a state belongs to, not just the state
        // description.
        embed_text: format!("{}: {}", s.entity_name, s.label),
        secondary_key: {
            let key = s.entity_name.trim().to_lowercase();
            if key.is_empty() {
                None
            } else {
                Some(key)
            }
        },
    }
}

fn from_relation_state(
    section: &SectionExtraction,
    ordinal: usize,
    index: usize,
    s: &RelationStateSketch,
) -> SketchHandle {
    SketchHandle {
        section_id: section.section_id.clone(),
        section_ordinal: ordinal,
        sketch_index: index,
        embed_text: format!("{} :: {}", s.participants.join(" × "), s.label),
        secondary_key: Some(participants_key(&s.participants)),
    }
}

fn from_event(
    section: &SectionExtraction,
    ordinal: usize,
    index: usize,
    e: &EventSketch,
) -> SketchHandle {
    SketchHandle {
        section_id: section.section_id.clone(),
        section_ordinal: ordinal,
        sketch_index: index,
        embed_text: e.description.clone(),
        secondary_key: Some(participants_key(&e.participants)),
    }
}

/// Unordered, case-folded participant set rendered as a canonical
/// string. Used as the secondary-signal key for relation states +
/// events.
fn participants_key(participants: &[String]) -> String {
    let mut names: Vec<String> = participants
        .iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    names.sort();
    names.dedup();
    names.join("|")
}

// ── Secondary signal merge pass ─────────────────────────────

/// Draft cluster as it flows through the merge pass. `label` is
/// the raw HDBSCAN label; `member_indices` are indices into the
/// per-facet sketches + embeddings lists; `secondary_signal` holds
/// the facet-specific keys that might link this cluster to another.
struct ClusterDraft {
    label: i32,
    member_indices: Vec<usize>,
    centroid: Vec<f32>,
    secondary_signal: Vec<SignalEntry>,
}

#[derive(Debug, Clone)]
enum SignalEntry {
    /// For claims — the attributed entity's case-folded name.
    /// Clusters sharing the same attributed entity are merge
    /// candidates.
    AttributedEntity(String),
    /// For events and relation states — the participant set.
    ParticipantSet(String),
    /// For entity states and relation states — the earliest /
    /// latest section ordinal bracket. Used to compute temporal
    /// proximity.
    SectionRange(usize, usize),
}

fn collect_secondary_signal(
    facet: Facet,
    members: &[usize],
    sketches: &[SketchHandle],
) -> Vec<SignalEntry> {
    let mut entries = Vec::new();
    match facet {
        Facet::Claim => {
            let mut attributions: HashSet<String> = HashSet::new();
            for &i in members {
                if let Some(k) = sketches[i].secondary_key.as_ref() {
                    attributions.insert(k.clone());
                }
            }
            for a in attributions {
                entries.push(SignalEntry::AttributedEntity(a));
            }
        }
        Facet::RelationState | Facet::Event => {
            let mut sets: HashSet<String> = HashSet::new();
            for &i in members {
                if let Some(k) = sketches[i].secondary_key.as_ref() {
                    sets.insert(k.clone());
                }
            }
            for s in sets {
                entries.push(SignalEntry::ParticipantSet(s));
            }
            if matches!(facet, Facet::RelationState) {
                entries.push(bracket_sections(members, sketches));
            }
        }
        Facet::EntityState => {
            // Temporal proximity only — entity-state clusters
            // that sit in near-neighbouring sections for the same
            // entity are the merge target.
            let mut entities: HashSet<String> = HashSet::new();
            for &i in members {
                if let Some(k) = sketches[i].secondary_key.as_ref() {
                    entities.insert(k.clone());
                }
            }
            for e in entities {
                entries.push(SignalEntry::AttributedEntity(e));
            }
            entries.push(bracket_sections(members, sketches));
        }
        Facet::Question => {
            // Spec §5.2: questions cluster on embedding similarity
            // only — no secondary signal.
        }
    }
    entries
}

fn bracket_sections(members: &[usize], sketches: &[SketchHandle]) -> SignalEntry {
    let (mut min, mut max) = (usize::MAX, 0usize);
    for &i in members {
        let o = sketches[i].section_ordinal;
        if o < min {
            min = o;
        }
        if o > max {
            max = o;
        }
    }
    if min == usize::MAX {
        SignalEntry::SectionRange(0, 0)
    } else {
        SignalEntry::SectionRange(min, max)
    }
}

fn find_secondary_signal_merges(clusters: &[ClusterDraft]) -> Vec<(usize, usize)> {
    let mut merges = Vec::new();
    for i in 0..clusters.len() {
        for j in (i + 1)..clusters.len() {
            if !secondary_signals_overlap(&clusters[i], &clusters[j]) {
                continue;
            }
            let sim = cosine_similarity(&clusters[i].centroid, &clusters[j].centroid);
            if sim >= SECONDARY_SIGNAL_MERGE_COSINE {
                merges.push((i, j));
            }
        }
    }
    merges
}

fn secondary_signals_overlap(a: &ClusterDraft, b: &ClusterDraft) -> bool {
    // Two clusters "share a signal" when an AttributedEntity or
    // ParticipantSet appears in both, OR their SectionRanges are
    // within STATE_TEMPORAL_PROXIMITY_SECTIONS of each other.
    // Question-facet clusters have no entries → always returns
    // false, which is exactly the behaviour §5.2 asks for.
    let mut attrs_a: HashSet<&str> = HashSet::new();
    let mut parts_a: HashSet<&str> = HashSet::new();
    let mut range_a: Option<(usize, usize)> = None;
    for e in &a.secondary_signal {
        match e {
            SignalEntry::AttributedEntity(s) => {
                attrs_a.insert(s.as_str());
            }
            SignalEntry::ParticipantSet(s) => {
                parts_a.insert(s.as_str());
            }
            SignalEntry::SectionRange(lo, hi) => {
                range_a = Some((*lo, *hi));
            }
        }
    }
    for e in &b.secondary_signal {
        match e {
            SignalEntry::AttributedEntity(s) => {
                if attrs_a.contains(s.as_str()) {
                    return true;
                }
            }
            SignalEntry::ParticipantSet(s) => {
                if parts_a.contains(s.as_str()) {
                    return true;
                }
            }
            SignalEntry::SectionRange(lo_b, hi_b) => {
                if let Some((lo_a, hi_a)) = range_a {
                    let gap = if hi_a < *lo_b {
                        lo_b.saturating_sub(hi_a)
                    } else if *hi_b < lo_a {
                        lo_a.saturating_sub(*hi_b)
                    } else {
                        0
                    };
                    if gap <= STATE_TEMPORAL_PROXIMITY_SECTIONS {
                        // But temporal proximity alone shouldn't
                        // merge — there must be a shared entity /
                        // participant too. Return false and let
                        // the attributed/participant scan be the
                        // positive signal.
                        continue;
                    }
                }
            }
        }
    }
    false
}

fn apply_merges(
    mut clusters: Vec<ClusterDraft>,
    mut merges: Vec<(usize, usize)>,
    embeddings: &[Vec<f32>],
) -> Vec<ClusterDraft> {
    if merges.is_empty() {
        return clusters;
    }
    // Disjoint-set union-find to fold chained merges into one
    // equivalence class each.
    let mut parent: Vec<usize> = (0..clusters.len()).collect();
    fn find(p: &mut Vec<usize>, x: usize) -> usize {
        if p[x] == x {
            x
        } else {
            let root = find(p, p[x]);
            p[x] = root;
            root
        }
    }
    // Sort merges for determinism.
    merges.sort();
    for (a, b) in merges {
        let ra = find(&mut parent, a);
        let rb = find(&mut parent, b);
        if ra != rb {
            parent[ra.max(rb)] = ra.min(rb);
        }
    }
    // Fold clusters into their root.
    let mut grouped: HashMap<usize, ClusterDraft> = HashMap::new();
    for (i, draft) in clusters.drain(..).enumerate() {
        let root = find(&mut parent, i);
        grouped
            .entry(root)
            .and_modify(|existing| {
                existing
                    .member_indices
                    .extend_from_slice(&draft.member_indices);
                existing
                    .secondary_signal
                    .extend(draft.secondary_signal.clone());
                // Recompute the centroid across the union.
                existing.centroid = centroid_of(&existing.member_indices, embeddings);
            })
            .or_insert(draft);
    }
    let mut merged: Vec<ClusterDraft> = grouped.into_values().collect();
    merged.sort_by_key(|c| c.label);
    merged
}

// ── Small math helpers ─────────────────────────────────────

fn centroid_of(members: &[usize], embeddings: &[Vec<f32>]) -> Vec<f32> {
    if members.is_empty() || embeddings.is_empty() {
        return Vec::new();
    }
    let dim = embeddings[members[0]].len();
    let mut acc = vec![0.0_f32; dim];
    for &i in members {
        for (a, &x) in acc.iter_mut().zip(embeddings[i].iter()) {
            *a += x;
        }
    }
    let n = members.len() as f32;
    for a in &mut acc {
        *a /= n;
    }
    acc
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0_f32, 0.0_f32, 0.0_f32);
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::pipeline::atlas::{
        ClaimSketch, DiscourseAct, EnrichmentDepth, EpistemicStatus, EventSketch,
    };
    use std::sync::Arc;

    fn cfg() -> ClusteringConfig {
        ClusteringConfig {
            min_cluster_size: 2,
            epsilon: 0.3,
            label_sample_size: 0,
            max_cluster_points: 0,
            reduced_dims: 0,
        }
    }

    /// Test embed: hashes the input's first 16 bytes into a 3-vec.
    /// Deterministic and close between similar strings.
    fn test_embed() -> EmbedFn {
        Arc::new(|s: &str| {
            let s = s.to_string();
            Box::pin(async move {
                let bytes = s.as_bytes();
                let a = bytes.first().copied().unwrap_or(0) as f32;
                let b = bytes.get(1).copied().unwrap_or(0) as f32;
                let c = bytes.get(2).copied().unwrap_or(0) as f32;
                Ok(vec![a, b, c])
            })
        })
    }

    /// Embed that always returns the same vector, forcing cosine = 1
    /// so the merge pass's cosine floor always passes.
    fn identical_embed() -> EmbedFn {
        Arc::new(|_s: &str| Box::pin(async move { Ok(vec![1.0_f32, 0.0, 0.0]) }))
    }

    fn claim(content: &str, attributed_to: Option<&str>) -> ClaimSketch {
        ClaimSketch {
            content: content.into(),
            discourse_act: DiscourseAct::Enact,
            epistemic_status: EpistemicStatus::Confident,
            attributed_to: attributed_to.map(String::from),
            anchor: String::new(),
            quotable_excerpt: None,
        }
    }

    #[tokio::test]
    async fn cluster_facet_partitions_by_facet_tag() {
        // Mix question + claim sketches across two sections. Each
        // facet clusters independently; the result carries its
        // facet tag.
        let sections = vec![
            SectionExtraction {
                section_id: "sec_0001".into(),
                enrichment_depth: EnrichmentDepth::Extracted,
                questions_raised: vec![
                    QuestionSketch {
                        content: "Can faith survive the world?".into(),
                        anchor: String::new(),
                    },
                    QuestionSketch {
                        content: "Can love remain untouched by suffering?".into(),
                        anchor: String::new(),
                    },
                ],
                claims: vec![
                    claim("Active love costs more than dreamt love.", None),
                    claim("The novel enacts the cost of transgression.", None),
                ],
                ..Default::default()
            },
            SectionExtraction {
                section_id: "sec_0002".into(),
                enrichment_depth: EnrichmentDepth::Extracted,
                questions_raised: vec![QuestionSketch {
                    content: "Can faith survive the world?".into(),
                    anchor: String::new(),
                }],
                claims: vec![claim("Active love costs more than dreamt love.", None)],
                ..Default::default()
            },
        ];
        let out = cluster_all_facets(&sections, &identical_embed(), &cfg())
            .await
            .unwrap();
        // One FacetClusterResult per populated facet (questions +
        // claims here; no states/relations/events in the fixture).
        let facets: Vec<Facet> = out.iter().map(|r| r.facet).collect();
        assert!(facets.contains(&Facet::Question));
        assert!(facets.contains(&Facet::Claim));
        assert!(!facets.contains(&Facet::EntityState));
        // Every produced cluster carries its own facet tag —
        // stored in the result, propagated into AtlasCluster.
        for result in &out {
            for cluster in &result.clusters {
                assert_eq!(cluster.facet, result.facet);
            }
        }
    }

    #[test]
    fn claim_clusters_use_attributed_entity_as_secondary_signal() {
        // Direct unit test on the merge helper. Two claim clusters
        // by the same attributed entity + centroid cosine ≥ 0.55
        // → merge candidate. This pins the attributed-entity
        // secondary signal without relying on HDBSCAN's density
        // judgment for a fixture-sized input.
        let clusters = vec![
            ClusterDraft {
                label: 0,
                member_indices: vec![0],
                centroid: vec![1.0, 0.0, 0.0],
                secondary_signal: vec![SignalEntry::AttributedEntity("zosima".into())],
            },
            ClusterDraft {
                label: 1,
                member_indices: vec![1],
                centroid: vec![0.9, 0.1, 0.0],
                secondary_signal: vec![SignalEntry::AttributedEntity("zosima".into())],
            },
        ];
        let merges = find_secondary_signal_merges(&clusters);
        assert_eq!(merges, vec![(0, 1)]);
    }

    #[test]
    fn claim_clusters_do_not_merge_across_attributions() {
        // Same centroid proximity but different attributed entity
        // → no merge. The secondary signal is load-bearing; it's
        // not just a "similar enough centroid" check.
        let clusters = vec![
            ClusterDraft {
                label: 0,
                member_indices: vec![0],
                centroid: vec![1.0, 0.0, 0.0],
                secondary_signal: vec![SignalEntry::AttributedEntity("zosima".into())],
            },
            ClusterDraft {
                label: 1,
                member_indices: vec![1],
                centroid: vec![0.99, 0.01, 0.0],
                secondary_signal: vec![SignalEntry::AttributedEntity("ivan".into())],
            },
        ];
        let merges = find_secondary_signal_merges(&clusters);
        assert!(merges.is_empty());
    }

    #[test]
    fn relation_state_clusters_honour_participant_overlap() {
        // Shared participant set (Jane+Rochester) + centroid
        // cosine ≥ 0.55 → merge. Participant set is unordered +
        // case-folded via `participants_key`, so "Jane|Rochester"
        // matches regardless of input order.
        let clusters = vec![
            ClusterDraft {
                label: 0,
                member_indices: vec![0],
                centroid: vec![1.0, 0.0, 0.0],
                secondary_signal: vec![SignalEntry::ParticipantSet("jane|rochester".into())],
            },
            ClusterDraft {
                label: 1,
                member_indices: vec![1],
                centroid: vec![0.9, 0.1, 0.0],
                secondary_signal: vec![SignalEntry::ParticipantSet("jane|rochester".into())],
            },
        ];
        let merges = find_secondary_signal_merges(&clusters);
        assert_eq!(merges, vec![(0, 1)]);
    }

    #[test]
    fn relation_state_clusters_do_not_merge_across_participant_sets() {
        let clusters = vec![
            ClusterDraft {
                label: 0,
                member_indices: vec![0],
                centroid: vec![1.0, 0.0, 0.0],
                secondary_signal: vec![SignalEntry::ParticipantSet("jane|rochester".into())],
            },
            ClusterDraft {
                label: 1,
                member_indices: vec![1],
                centroid: vec![1.0, 0.0, 0.0],
                secondary_signal: vec![SignalEntry::ParticipantSet("anna|vronsky".into())],
            },
        ];
        let merges = find_secondary_signal_merges(&clusters);
        assert!(merges.is_empty());
    }

    #[test]
    fn merge_respects_centroid_cosine_floor() {
        // Same attributed entity but centroid cosine too low to
        // merge — secondary signal alone isn't enough.
        let clusters = vec![
            ClusterDraft {
                label: 0,
                member_indices: vec![0],
                centroid: vec![1.0, 0.0, 0.0],
                secondary_signal: vec![SignalEntry::AttributedEntity("zosima".into())],
            },
            ClusterDraft {
                label: 1,
                member_indices: vec![1],
                centroid: vec![0.3, 0.95, 0.0], // ~0.3 cosine
                secondary_signal: vec![SignalEntry::AttributedEntity("zosima".into())],
            },
        ];
        let merges = find_secondary_signal_merges(&clusters);
        assert!(merges.is_empty());
    }

    #[test]
    fn end_to_end_cluster_facet_end_to_end_on_real_relation_sketches() {
        // Integration-style: feed relation-state sketches through
        // cluster_facet with test_embed. The test asserts the
        // facet plumbing delivers a non-empty output carrying the
        // correct facet tag and keeping every sketch accounted
        // for (clustered or noise).
        fn rs(label: &str) -> RelationStateSketch {
            RelationStateSketch {
                participants: vec!["Jane".into(), "Rochester".into()],
                label: label.into(),
                anchor: String::new(),
            }
        }
        let sections = vec![SectionExtraction {
            section_id: "sec_0001".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            relations_developed: vec![rs("Adversarial testing"), rs("Adversarial sparring")],
            ..Default::default()
        }];
        let rt = tokio::runtime::Runtime::new().unwrap();
        let out = rt
            .block_on(cluster_facet(
                &sections,
                Facet::RelationState,
                &test_embed(),
                &cfg(),
            ))
            .unwrap();
        assert_eq!(out.facet, Facet::RelationState);
        let total_refs: usize =
            out.clusters.iter().map(|c| c.refs.len()).sum::<usize>() + out.unclustered.len();
        assert_eq!(total_refs, 2);
    }

    #[tokio::test]
    async fn question_clusters_use_embedding_only_no_secondary_signal() {
        // Even when several questions share a word, only embedding
        // cosine decides whether they cluster. A question has no
        // attributed_to / participants, so the secondary signal
        // path produces zero entries.
        let sections = vec![SectionExtraction {
            section_id: "sec_0001".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            questions_raised: vec![
                QuestionSketch {
                    content: "A question about love.".into(),
                    anchor: String::new(),
                },
                QuestionSketch {
                    content: "A question about faith.".into(),
                    anchor: String::new(),
                },
            ],
            ..Default::default()
        }];
        // With distinct seeds the fake embedder gives slightly
        // different vectors, HDBSCAN puts them in the same cluster
        // (min_cluster_size=2 and both land close). Either way,
        // the test pins: no panic, results carry Question facet.
        let out = cluster_facet(&sections, Facet::Question, &test_embed(), &cfg())
            .await
            .unwrap();
        assert_eq!(out.facet, Facet::Question);
        for cluster in &out.clusters {
            assert_eq!(cluster.facet, Facet::Question);
        }
    }

    #[tokio::test]
    async fn cluster_all_facets_skips_empty_facets() {
        // A section with only questions should return just one
        // FacetClusterResult — the Question one — because the
        // other facets have no sketches.
        let sections = vec![SectionExtraction {
            section_id: "sec_0001".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            questions_raised: vec![
                QuestionSketch {
                    content: "a".into(),
                    anchor: String::new(),
                },
                QuestionSketch {
                    content: "b".into(),
                    anchor: String::new(),
                },
            ],
            ..Default::default()
        }];
        let out = cluster_all_facets(&sections, &test_embed(), &cfg())
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].facet, Facet::Question);
    }

    #[tokio::test]
    async fn events_dedupe_noise_lists_refs_with_facet_tag() {
        // A single event becomes noise because min_cluster_size=2
        // and there's only one event sketch. The result's
        // `unclustered` list carries the ref with facet=Event.
        fn ev(desc: &str) -> EventSketch {
            EventSketch {
                description: desc.into(),
                participants: vec!["Alyosha".into()],
                anchor: String::new(),
            }
        }
        let sections = vec![SectionExtraction {
            section_id: "sec_0001".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            events: vec![ev("a lone event")],
            ..Default::default()
        }];
        let out = cluster_facet(&sections, Facet::Event, &test_embed(), &cfg())
            .await
            .unwrap();
        assert!(out.clusters.is_empty());
        assert_eq!(out.unclustered.len(), 1);
        assert_eq!(out.unclustered[0].facet, Facet::Event);
        assert_eq!(out.unclustered[0].sketch_index, 0);
    }

    #[test]
    fn participants_key_is_unordered_and_case_folded() {
        let k1 = participants_key(&["Jane".into(), "Rochester".into()]);
        let k2 = participants_key(&["ROCHESTER".into(), "jane".into()]);
        assert_eq!(k1, k2);
        // Empty participants drop out.
        let k3 = participants_key(&["".into(), "Jane".into(), "  ".into()]);
        assert_eq!(k3, "jane");
    }
}
