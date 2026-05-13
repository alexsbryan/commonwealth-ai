//! Content-derived stable key for atlas atoms.
//!
//! ## Why this exists
//!
//! `AtomId` (`entity-0001`, `claim-0042`…) is **sequential**, assigned
//! by Phase 3a/3b resolution — see
//! `corpus-engine/src/enrichment/atlas/atoms.rs:29-52`. Re-running
//! extraction with even slightly different ordering renumbers every
//! atom. Anything that wants to refer to an atom *across re-extractions*
//! — Phase 2's curation overlay, external bookmarks, cross-session UI
//! state — needs a key derived from the atom's content, not its
//! position in the list.
//!
//! ## What's hashed
//!
//! `blake3(corpus_id || US || atom_type || US || disambiguator || US || anchor)`
//!
//! where `US` is `\x1f` (unit separator) and:
//!
//! | Atom type              | Disambiguator        | Anchor                            |
//! |------------------------|----------------------|-----------------------------------|
//! | Entity                 | `canonical_name`     | `first_appearance.chunk_id`       |
//! | Event                  | `description`        | `section_position.section_id`     |
//! | State                  | `label`              | `section_range.start`             |
//! | Relation               | `label`              | `section_range.start`             |
//! | Claim                  | `content`            | first `evidence.chunk_id` or `""` |
//! | Question               | `content`            | first `raised_at.chunk_id` or `""`|
//! | Configuration          | `label`              | first `evidence.chunk_id` or `""` |
//! | ArgumentReconstruction | `name`               | `section_position.section_id`     |
//!
//! ## Caveat — the weakest link
//!
//! Chunk IDs themselves can drift on re-ingestion (re-chunking, section
//! renaming). Perfect stability requires hashing the evidence passage
//! *content*, not its location. That's heavier — defer until overlay
//! write rates justify the cost. For now, the (corpus_id, atom_type,
//! disambiguator) prefix carries most of the stability; the anchor
//! disambiguates same-named atoms within the same corpus.

use corpus_engine::enrichment::atlas::atoms::{AtomEnvelope, ChunkRef};
use serde::{Deserialize, Serialize};

/// Content-derived stable identifier for an atom. Hex-encoded blake3.
///
/// Use this as the persistence key for anything that wants to outlive
/// a re-extraction (Phase 2 overlay, UI bookmarks, eval pin lists).
/// `AtomId` is fine for in-session references where re-extraction
/// hasn't happened.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StableAtomKey(String);

impl StableAtomKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Build from a raw hex string. Callers should normally use
    /// [`compute_stable_key`] — this exists for deserialisation and
    /// for tests that want to assert against a known value.
    pub fn from_hex(hex: impl Into<String>) -> Self {
        Self(hex.into())
    }
}

impl std::fmt::Display for StableAtomKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

const US: &[u8] = b"\x1f";

/// Compute the stable key for an atom in a given corpus. See module
/// docs for the field-selection table.
pub fn compute_stable_key(corpus_id: &str, atom: &AtomEnvelope) -> StableAtomKey {
    let (atom_type_tag, disambiguator, anchor) = match atom {
        AtomEnvelope::Entity(a) => (
            "Entity",
            a.canonical_name.as_str(),
            a.first_appearance.chunk_id.as_str(),
        ),
        AtomEnvelope::Event(a) => (
            "Event",
            a.description.as_str(),
            a.section_position.section_id.as_str(),
        ),
        AtomEnvelope::State(a) => ("State", a.label.as_str(), a.section_range.start.as_str()),
        AtomEnvelope::Relation(a) => {
            ("Relation", a.label.as_str(), a.section_range.start.as_str())
        }
        AtomEnvelope::Claim(a) => ("Claim", a.content.as_str(), first_chunk_id(&a.evidence)),
        AtomEnvelope::Question(a) => (
            "Question",
            a.content.as_str(),
            first_chunk_id(&a.raised_at),
        ),
        AtomEnvelope::Configuration(a) => {
            ("Configuration", a.label.as_str(), first_chunk_id(&a.evidence))
        }
        AtomEnvelope::ArgumentReconstruction(a) => (
            "ArgumentReconstruction",
            a.name.as_str(),
            a.section_position.section_id.as_str(),
        ),
    };

    let mut h = blake3::Hasher::new();
    h.update(corpus_id.as_bytes());
    h.update(US);
    h.update(atom_type_tag.as_bytes());
    h.update(US);
    h.update(disambiguator.as_bytes());
    h.update(US);
    h.update(anchor.as_bytes());
    StableAtomKey(h.finalize().to_hex().to_string())
}

fn first_chunk_id(refs: &[ChunkRef]) -> &str {
    refs.first().map(|r| r.chunk_id.as_str()).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::enrichment::atlas::atoms::{
        AtomEnvelope, AtomId, ChunkRef, Claim, Entity, SectionRange, State,
    };
    use corpus_engine::enrichment::pipeline::atlas::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EntityType, EpistemicStatus, StateType,
    };

    fn sample_entity(canonical: &str, first_chunk: &str) -> AtomEnvelope {
        AtomEnvelope::Entity(Entity {
            id: AtomId::entity(1),
            canonical_name: canonical.into(),
            aliases: vec![],
            entity_type: EntityType::Concept,
            first_appearance: ChunkRef::new(first_chunk, None),
            description: "a description".into(),
            defining_quote: None,
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: vec![],
        })
    }

    #[test]
    fn stable_key_is_deterministic() {
        let atom = sample_entity("Justice", "ch001");
        let k1 = compute_stable_key("wikipedia", &atom);
        let k2 = compute_stable_key("wikipedia", &atom);
        assert_eq!(k1, k2);
        // Sanity: blake3 hex is 64 chars.
        assert_eq!(k1.as_str().len(), 64);
    }

    #[test]
    fn stable_key_differs_across_corpora() {
        let atom = sample_entity("Justice", "ch001");
        let k1 = compute_stable_key("wikipedia", &atom);
        let k2 = compute_stable_key("sep-political-philosophy", &atom);
        assert_ne!(k1, k2, "same atom in different corpora must hash differently");
    }

    #[test]
    fn stable_key_distinguishes_same_name_different_evidence() {
        // Two entities both named "Justice" but anchored to different
        // first-appearance chunks should hash differently. Defends
        // against the rare-but-real case of two distinct Entity atoms
        // sharing a canonical_name within a single corpus.
        let a = sample_entity("Justice", "ch001");
        let b = sample_entity("Justice", "ch042");
        assert_ne!(compute_stable_key("wikipedia", &a), compute_stable_key("wikipedia", &b));
    }

    #[test]
    fn stable_key_stable_under_atom_id_renumber() {
        // The whole point: renumbering AtomId must not change the
        // stable key, because Phase 2's overlay would otherwise
        // orphan on every re-extraction.
        let mut a = sample_entity("Justice", "ch001");
        let mut b = sample_entity("Justice", "ch001");
        if let AtomEnvelope::Entity(ref mut e) = a {
            e.id = AtomId::entity(7);
        }
        if let AtomEnvelope::Entity(ref mut e) = b {
            e.id = AtomId::entity(9999);
        }
        assert_eq!(
            compute_stable_key("wikipedia", &a),
            compute_stable_key("wikipedia", &b),
            "atom_id must not influence stable_key",
        );
    }

    #[test]
    fn stable_key_distinguishes_atom_types() {
        // An Entity named "Justice" and a State labelled "Justice"
        // sharing the same anchor must not collide.
        let entity = sample_entity("Justice", "ch001");
        let state = AtomEnvelope::State(State {
            id: AtomId::state(1),
            entity_id: AtomId::entity(1),
            label: "Justice".into(),
            state_type: StateType::Psychological,
            evidence: vec![],
            section_range: SectionRange::point("ch001"),
            confidence: None,
            enrichment_depth: EnrichmentDepth::Extracted,
        });
        assert_ne!(
            compute_stable_key("wikipedia", &entity),
            compute_stable_key("wikipedia", &state),
        );
    }

    #[test]
    fn stable_key_handles_claim_without_evidence() {
        // Claims may serialise with empty evidence (deterministic
        // resolver path). Anchor falls back to "". Must still hash
        // deterministically and disambiguate by content.
        let claim_a = AtomEnvelope::Claim(Claim {
            id: AtomId::claim(1),
            content: "Knowledge is justified true belief.".into(),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: vec![],
            quotable_excerpt: None,
            attributed_to: None,
            confidence: None,
            enrichment_depth: EnrichmentDepth::Extracted,
        });
        let claim_b = AtomEnvelope::Claim(Claim {
            id: AtomId::claim(2),
            content: "Knowledge is more than justified true belief.".into(),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: vec![],
            quotable_excerpt: None,
            attributed_to: None,
            confidence: None,
            enrichment_depth: EnrichmentDepth::Extracted,
        });
        let k_a = compute_stable_key("sep-epistemology", &claim_a);
        let k_b = compute_stable_key("sep-epistemology", &claim_b);
        assert_eq!(k_a.as_str().len(), 64);
        assert_ne!(k_a, k_b);
    }

    #[test]
    fn stable_key_unit_separator_prevents_field_concatenation_collision() {
        // Without the unit separator, ("Justice", "ch001") and
        // ("Justicec", "h001") would hash the same. Pin that the
        // separator does its job.
        let a = sample_entity("Justice", "ch001");
        let b = sample_entity("Justicec", "h001");
        assert_ne!(compute_stable_key("wikipedia", &a), compute_stable_key("wikipedia", &b));
    }
}
