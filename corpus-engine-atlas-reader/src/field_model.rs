// SPDX-License-Identifier: AGPL-3.0-or-later
//! The field model's READ half — where a corpus's field model lives and the
//! skeleton VIEW rebuilt from atlas atoms. Carved from corpus-engine's
//! `enrichment::field_atoms` by fp-60 (FIVE_PROGRAMS §12 decision 1); the
//! projection and the publish (`skeleton_to_atoms`, `publish_to_atlas`) stay
//! there, and corpus-engine re-exports these items at their historical paths.

use understanding_vocab::atoms::{AtomEnvelope, Opposition, Position, ResolutionStatus};
use understanding_vocab::skeleton::{
    CanonicalQuestion, FieldModelStats, FieldSkeleton, SkeletonFaultLine, SkeletonOpenQuestion,
    SkeletonPosition,
};

/// `extraction_method` on a skeleton VIEW rebuilt from atoms — so a reader who
/// dumps one can tell it from a v1 file that was parsed off disk.
pub const ATOM_SOURCED_METHOD: &str = "atlas_atoms_v1";

/// The status string a `ResolutionStatus::Open` question reads back with.
/// `is_settled_status` must not match it, and the two v1 lists must partition.
pub const OPEN_STATUS: &str = "open";

/// Rebuild the skeleton VIEW `render_landscape` reads, from atlas atoms.
///
/// This is not a v1 file and does not claim to be: the provenance fields say
/// [`ATOM_SOURCED_METHOD`] and `field_stats` is zeroed, because the atoms carry
/// no clustering census. Everything `render_landscape` actually reads —
/// question text, position statuses, fault-line cruxes, open questions — is
/// rebuilt exactly.
///
/// Atoms that are not `Question` / `Position` / `Opposition` are ignored, so
/// this reads correctly against a full atlas that also holds entities, claims
/// and summaries.
pub fn skeleton_from_atoms(corpus_id: &str, atoms: &[AtomEnvelope]) -> FieldSkeleton {
    // Positions and oppositions are looked up by id / by the question they
    // follow; build the position index first so a Question can resolve its
    // `addressed_by` regardless of write order.
    let mut positions: std::collections::HashMap<&str, &Position> =
        std::collections::HashMap::new();
    for a in atoms {
        if let AtomEnvelope::Position(p) = a {
            positions.insert(p.id.as_str(), p);
        }
    }
    let oppositions: Vec<&Opposition> = atoms
        .iter()
        .filter_map(|a| match a {
            AtomEnvelope::Opposition(o) => Some(o),
            _ => None,
        })
        .collect();
    // Every fault line the v1 skeleton carried hung off SOME canonical
    // question, and the digest flattens them across all of them anyway
    // (`render_landscape` chains `q.fault_lines`). Hanging them all on the
    // first canonical question reproduces that flattened list in order without
    // inventing a question→opposition edge the atoms do not carry.
    let mut pending_fault_lines: Vec<SkeletonFaultLine> = oppositions
        .iter()
        .map(|o| SkeletonFaultLine {
            id: o.id.as_str().to_string(),
            between_positions: vec![o.left_label.clone(), o.right_label.clone()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect(),
            crux: if o.framing.is_empty() {
                o.canonical_label.clone()
            } else {
                o.framing.clone()
            },
            key_chunk_ids: Vec::new(),
            confidence: o.salience,
            source: "atlas".to_string(),
            resolution_condition: None,
        })
        .collect();

    let mut canonical_questions: Vec<CanonicalQuestion> = Vec::new();
    let mut open_questions: Vec<SkeletonOpenQuestion> = Vec::new();

    for a in atoms {
        let AtomEnvelope::Question(q) = a else {
            continue;
        };
        if matches!(q.resolution_status, ResolutionStatus::Open) {
            open_questions.push(SkeletonOpenQuestion {
                id: q.id.as_str().to_string(),
                question: q.content.clone(),
                status: OPEN_STATUS.to_string(),
                question_type: Some(q.question_type.as_str_repr().to_string()),
                related_question_id: None,
                representative_chunk_ids: Vec::new(),
            });
            continue;
        }
        canonical_questions.push(CanonicalQuestion {
            id: q.id.as_str().to_string(),
            question: q.content.clone(),
            status: resolution_to_canonical_status(&q.resolution_status).to_string(),
            question_type: q.question_type.as_str_repr().to_string(),
            primary_entries: Vec::new(),
            positions: q
                .addressed_by
                .iter()
                .filter_map(|id| positions.get(id.as_str()))
                .map(|p| SkeletonPosition {
                    id: p.id.as_str().to_string(),
                    name: p.canonical_name.clone(),
                    claim: p.content.clone(),
                    status: p.stance.clone(),
                    proponents: Vec::new(),
                    source: "atlas".to_string(),
                    cluster_ids: Vec::new(),
                    centroid_chunk_ids: Vec::new(),
                    discovery_confidence: None,
                })
                .collect(),
            fault_lines: Vec::new(),
        });
    }
    if let Some(first) = canonical_questions.first_mut() {
        first.fault_lines.append(&mut pending_fault_lines);
    }

    FieldSkeleton {
        schema_version: 1,
        corpus_id: corpus_id.to_string(),
        generated_at: String::new(),
        extraction_method: ATOM_SOURCED_METHOD.to_string(),
        prompt_version: String::new(),
        domain_id: String::new(),
        canonical_questions,
        open_questions,
        field_stats: FieldModelStats::default(),
    }
}

/// The inverse of corpus-engine's `field_atoms::canonical_status_to_resolution`
/// for the two values a canonical question can hold. `Open` never reaches here
/// — it is partitioned off before the call.
fn resolution_to_canonical_status(status: &ResolutionStatus) -> &'static str {
    match status {
        ResolutionStatus::Resolved { .. } => "resolved",
        ResolutionStatus::Contested { .. } => "contested",
        ResolutionStatus::Dissolved => "dissolved",
        ResolutionStatus::Open => OPEN_STATUS,
    }
}

/// inferred: "the digest is empty" and "the digest came from the legacy file"
/// are different facts and an operator has to be able to tell them apart
/// (ARCH §18.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldModelSource {
    /// `atlas/atoms.json` — the ported shape.
    Atlas,
    /// `field_skeleton.json` — a corpus enriched before ei-7b whose atoms have
    /// not been published yet. Run `svrn enrich field-atoms <corpus>` to move
    /// it.
    LegacyFile,
}

impl FieldModelSource {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldModelSource::Atlas => "atlas",
            FieldModelSource::LegacyFile => "legacy_file",
        }
    }
}

/// Load a corpus's field model, from wherever it actually is.
///
/// THE ONE ACCESSOR for that question (ARCH §10.6). `turn_prepass`'s ambient
/// digest is its caller today; the KnowledgeView manager is the next one when
/// its reader is ported.
///
/// Precedence, and why it is this way round:
///
/// 1. **The atlas**, when it carries canonical questions. The ported shape
///    wins wherever it exists, so migrating a corpus is the whole switch and
///    nothing has to be turned off afterwards.
/// 2. **`field_skeleton.json`**, when the atlas has no field model and the
///    v1 file is still on disk. This is a MIGRATION FALLBACK and it is the
///    reason the port can land without taking a corpus's digest dark: `sep`
///    carries 549 canonical questions in that file and an EMPTY atlas, so an
///    atlas-only reader would silently stop splicing its Field guide. Which
///    source a corpus uses is therefore a DATA choice — run
///    `svrn enrich field-atoms <corpus>` and it moves — not a code or config
///    one.
/// 3. **`None`** when neither has a field model. The caller splices nothing.
///
/// Cost. Reading a whole `atlas/atoms.json` on every turn is a much bigger
/// parse than the v1 file was on a large atlas, so the atlas's own census
/// (`_summary.json`, written by the atlas writer) is consulted first and the
/// atlas is skipped without being read when it reports zero `Question` atoms.
/// A census that is ABSENT or STALE is NOT read as "no questions" — that falls
/// through to the full read, which is correct and merely slower.
pub fn load_field_model(
    index_dir: &std::path::Path,
    corpus_id: &str,
) -> Option<(FieldSkeleton, FieldModelSource)> {
    use crate::summary::read_current_summary as read_current_atlas_summary;
    use understanding_vocab::atoms::AtomType;
    use understanding_vocab::read::{read_atlas_atoms, ATLAS_DIRNAME};

    let atlas_dir = index_dir.join(ATLAS_DIRNAME);
    let census_says_none = read_current_atlas_summary(&atlas_dir)
        .is_some_and(|c| c.atom_counts.get(&AtomType::Question).copied().unwrap_or(0) == 0);
    if !census_says_none {
        match read_atlas_atoms(&atlas_dir) {
            Ok(file) => {
                let view = skeleton_from_atoms(corpus_id, &file.atoms());
                if !view.is_empty() {
                    return Some((view, FieldModelSource::Atlas));
                }
            }
            Err(e) => {
                tracing::debug!(
                    corpus = %corpus_id,
                    atlas_dir = %atlas_dir.display(),
                    error = %e,
                    "field model: no readable atlas — falling back to the v1 file if there is one"
                );
            }
        }
    }

    let legacy = index_dir.join(LEGACY_ARTIFACT);
    if !legacy.exists() {
        return None;
    }
    match std::fs::read_to_string(&legacy)
        .map_err(|e| e.to_string())
        .and_then(|raw| serde_json::from_str::<FieldSkeleton>(&raw).map_err(|e| e.to_string()))
    {
        Ok(skel) if !skel.is_empty() => {
            tracing::debug!(
                corpus = %corpus_id,
                questions = skel.canonical_questions.len(),
                "field model: read from the v1 file — this corpus has not been migrated \
                 (run `svrn enrich field-atoms` to publish its atoms)"
            );
            Some((skel, FieldModelSource::LegacyFile))
        }
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(
                corpus = %corpus_id,
                path = %legacy.display(),
                error = %e,
                "field model: v1 file present but unreadable — reporting no field model"
            );
            None
        }
    }
}

/// The pre-ei-7b artifact name. One spelling, here, so the fallback and the
/// index accessor cannot disagree about which file they mean — corpus-engine's
/// `index::field_skeleton::FIELD_SKELETON_FILENAME` re-exports it.
pub const LEGACY_ARTIFACT: &str = "field_skeleton.json";

/// Load the field skeleton JSON artifact if it exists.
///
/// Readers: the KnowledgeView manager and its cross-view digest, the
/// desktop budget probe, `sovereign-tools::epistemic`, the one-shot
/// `enrich field-atoms` migration, and corpus-engine's `load_field_checkpoint`
/// fallback. For an `AtlasAtoms` domain this file is a pre-ei-7b leftover
/// and the live field model is in the atlas.
pub fn load_field_skeleton(
    dir: &std::path::Path,
) -> corpus_index::Result<Option<FieldSkeleton>> {
    read_skeleton_json(&dir.join(LEGACY_ARTIFACT))
}

/// Read one skeleton JSON file; `None` when it does not exist.
pub fn read_skeleton_json(path: &std::path::Path) -> corpus_index::Result<Option<FieldSkeleton>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)?;
    let skeleton = serde_json::from_str(&raw).map_err(|e| {
        corpus_index::Error::Serialization(format!("Bad field skeleton at {}: {e}", path.display()))
    })?;
    Ok(Some(skeleton))
}
