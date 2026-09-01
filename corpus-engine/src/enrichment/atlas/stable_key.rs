// SPDX-License-Identifier: AGPL-3.0-or-later
//! Content-derived stable identity for atlas atoms.
//!
//! ## Why this exists
//!
//! [`AtomId`](super::atoms::AtomId)'s sequential shape (`entity-0001`,
//! `claim-0042`…) is assigned by Phase 3a/3b resolution. Re-running
//! extraction with even slightly different ordering renumbers every atom.
//! Anything that wants to refer to an atom *across re-extractions* — the
//! curation overlay, external bookmarks, cross-session UI state, eval pin
//! lists — needs a key derived from the atom's content, not from its position
//! in the list. That is ARCH §7.5: identity comes from essence, never from a
//! counter.
//!
//! ## Why it lives here
//!
//! It answers a question about the atom, by reading fields only the atom's
//! variants have. It lived in `sovereign-tools`' `atlas_view` until
//! 2026-08-20, where it re-derived the identity of a noun it does not own
//! through an eleven-arm match on `AtomEnvelope`'s private variant fields —
//! the same shape as the five fan-outs that became accessors on the envelope
//! earlier that day. A second crate deciding what makes two atoms "the same
//! atom" is two deciders for one identity (ARCH §10.6), and a new atom kind
//! added here would have silently acquired a key derived from nothing.
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
//! | Position               | `canonical_name`     | `first_appearance.chunk_id`       |
//! | Opposition             | `canonical_label`    | `first_appearance.chunk_id`       |
//! | Asset                  | `sha256`             | `first_seen_source_doc_id`        |
//!
//! The unit separator is load-bearing: without it `("Justice", "ch001")` and
//! `("Justicec", "h001")` would hash identically. A test pins that.
//!
//! ## Caveat — the weakest link
//!
//! Chunk IDs themselves can drift on re-ingestion (re-chunking, section
//! renaming). Perfect stability requires hashing the evidence passage
//! *content*, not its location. That's heavier — deferred until overlay write
//! rates justify the cost. For now the `(corpus_id, atom_type, disambiguator)`
//! prefix carries most of the stability; the anchor disambiguates same-named
//! atoms within the same corpus.

use serde::{Deserialize, Serialize};

use super::atoms::{AtomEnvelope, ChunkRef};

/// Content-derived stable identifier for an atom. Hex-encoded blake3.
///
/// Use this as the persistence key for anything that wants to outlive a
/// re-extraction (curation overlay, UI bookmarks, eval pin lists).
/// [`AtomId`](super::atoms::AtomId) is fine for in-session references where
/// re-extraction hasn't happened.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StableAtomKey(String);

impl StableAtomKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Build from a raw hex string. Callers should normally use
    /// [`AtomEnvelope::stable_key`] — this exists for deserialisation and for
    /// tests that want to assert against a known value.
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

impl AtomEnvelope {
    /// This atom's content-derived identity within `corpus_id` — the key that
    /// survives re-extraction's `AtomId` renumbering. See the module docs for
    /// the field-selection table.
    ///
    /// The single canonical stable-key derivation. Callers must use this
    /// rather than re-matching the variants, so a new atom kind cannot acquire
    /// a second, divergent identity — the same discipline as
    /// [`AtomEnvelope::atom_type`] and [`AtomEnvelope::evidence`].
    pub fn stable_key(&self, corpus_id: &str) -> StableAtomKey {
        let (atom_type_tag, disambiguator, anchor) = match self {
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
            AtomEnvelope::Question(a) => {
                ("Question", a.content.as_str(), first_chunk_id(&a.raised_at))
            }
            AtomEnvelope::Configuration(a) => (
                "Configuration",
                a.label.as_str(),
                first_chunk_id(&a.evidence),
            ),
            AtomEnvelope::ArgumentReconstruction(a) => (
                "ArgumentReconstruction",
                a.name.as_str(),
                a.section_position.section_id.as_str(),
            ),
            AtomEnvelope::Position(a) => (
                "Position",
                a.canonical_name.as_str(),
                a.first_appearance.chunk_id.as_str(),
            ),
            AtomEnvelope::Opposition(a) => (
                "Opposition",
                a.canonical_label.as_str(),
                a.first_appearance.chunk_id.as_str(),
            ),
            AtomEnvelope::Asset(a) => (
                "Asset",
                a.sha256.as_str(),
                a.first_seen_source_doc_id.as_str(),
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
}

fn first_chunk_id(refs: &[ChunkRef]) -> &str {
    refs.first().map(|r| r.chunk_id.as_str()).unwrap_or("")
}

#[cfg(test)]
mod tests {
    
    use crate::enrichment::atlas::atoms::{
        AtomEnvelope, AtomId, ChunkRef, Claim, Entity, Event, Question, ResolutionStatus,
        SectionPosition, SectionRange, State,
    };
    use crate::enrichment::pipeline::atlas::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EntityType, EpistemicStatus, EventType,
        QuestionType, StateType,
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
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        })
    }

    #[test]
    fn stable_key_is_deterministic() {
        let atom = sample_entity("Justice", "ch001");
        let k1 = atom.stable_key("wikipedia");
        let k2 = atom.stable_key("wikipedia");
        assert_eq!(k1, k2);
        // Sanity: blake3 hex is 64 chars.
        assert_eq!(k1.as_str().len(), 64);
    }

    #[test]
    fn stable_key_differs_across_corpora() {
        let atom = sample_entity("Justice", "ch001");
        assert_ne!(
            atom.stable_key("wikipedia"),
            atom.stable_key("sep-political-philosophy"),
            "same atom in different corpora must hash differently"
        );
    }

    #[test]
    fn stable_key_distinguishes_same_name_different_evidence() {
        // Two entities both named "Justice" but anchored to different
        // first-appearance chunks should hash differently. Defends against the
        // rare-but-real case of two distinct Entity atoms sharing a
        // canonical_name within a single corpus.
        let a = sample_entity("Justice", "ch001");
        let b = sample_entity("Justice", "ch042");
        assert_ne!(a.stable_key("wikipedia"), b.stable_key("wikipedia"));
    }

    #[test]
    fn stable_key_stable_under_atom_id_renumber() {
        // The whole point: renumbering AtomId must not change the stable key,
        // because the curation overlay would otherwise orphan on every
        // re-extraction.
        let mut a = sample_entity("Justice", "ch001");
        let mut b = sample_entity("Justice", "ch001");
        if let AtomEnvelope::Entity(ref mut e) = a {
            e.id = AtomId::entity(7);
        }
        if let AtomEnvelope::Entity(ref mut e) = b {
            e.id = AtomId::entity(9999);
        }
        assert_eq!(
            a.stable_key("wikipedia"),
            b.stable_key("wikipedia"),
            "atom_id must not influence stable_key",
        );
    }

    #[test]
    fn stable_key_distinguishes_atom_types() {
        // An Entity named "Justice" and a State labelled "Justice" sharing the
        // same anchor must not collide.
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
            entity.stable_key("wikipedia"),
            state.stable_key("wikipedia")
        );
    }

    #[test]
    fn stable_key_handles_claim_without_evidence() {
        // Claims may serialise with empty evidence (deterministic resolver
        // path). Anchor falls back to "". Must still hash deterministically
        // and disambiguate by content.
        let claim = |id: usize, content: &str| {
            AtomEnvelope::Claim(Claim {
                id: AtomId::claim(id),
                content: content.into(),
                discourse_act: DiscourseAct::Assert,
                epistemic_status: EpistemicStatus::Confident,
                scope: ClaimScope::Universal,
                evidence: vec![],
                quotable_excerpt: None,
                attributed_to: None,
                confidence: None,
                anchor: None,
                enrichment_depth: EnrichmentDepth::Extracted,
                claim_kind: None,
                concession_outcome: None,
                evidence_kind: None,
            })
        };
        let k_a = claim(1, "Knowledge is justified true belief.").stable_key("sep-epistemology");
        let k_b = claim(2, "Knowledge is more than justified true belief.")
            .stable_key("sep-epistemology");
        assert_eq!(k_a.as_str().len(), 64);
        assert_ne!(k_a, k_b);
    }

    // ── referenced_atom_ids ──────────────────────────────────
    //
    // These live beside the stable-key tests because both accessors answer a
    // structural question about the atom that a second crate used to answer by
    // matching the variants itself.

    #[test]
    fn referenced_ids_chain_every_reference_field_of_a_kind() {
        // Event carries atom ids in TWO fields. A fan-out that reads only
        // `participants` loses the causal chain, and nothing else would notice.
        let event = AtomEnvelope::Event(Event {
            id: AtomId::event(1),
            description: "the trial".into(),
            event_type: EventType::Decision,
            participants: vec![AtomId::entity(1), AtomId::entity(2)],
            evidence: vec![],
            section_position: SectionPosition::section("sec-1"),
            causal_antecedents: vec![AtomId::event(9)],
            enrichment_depth: EnrichmentDepth::Extracted,
        });
        let ids: Vec<&str> = event
            .referenced_atom_ids()
            .iter()
            .map(|i| i.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["entity-0001", "entity-0002", "event-0009"],
            "participants AND causal antecedents are both references",
        );
    }

    #[test]
    fn referenced_ids_follow_question_resolution_into_every_status() {
        // `ResolutionStatus` hides atom ids inside two of its four variants.
        // Each status must contribute the claims it names, and the two that
        // name none must contribute none.
        let question = |status: ResolutionStatus| {
            AtomEnvelope::Question(Question {
                id: AtomId::question(1),
                content: "what is justice?".into(),
                question_type: QuestionType::Thematic,
                addressed_by: vec![AtomId::claim(1)],
                raised_at: vec![],
                resolution_status: status,
                enrichment_depth: EnrichmentDepth::Extracted,
            })
        };
        let ids = |a: AtomEnvelope| -> Vec<String> {
            a.referenced_atom_ids()
                .iter()
                .map(|i| i.as_str().to_string())
                .collect()
        };

        assert_eq!(ids(question(ResolutionStatus::Open)), vec!["claim-0001"]);
        assert_eq!(
            ids(question(ResolutionStatus::Dissolved)),
            vec!["claim-0001"]
        );
        assert_eq!(
            ids(question(ResolutionStatus::Resolved {
                claim_id: AtomId::claim(7)
            })),
            vec!["claim-0001", "claim-0007"],
        );
        assert_eq!(
            ids(question(ResolutionStatus::Contested {
                claim_ids: vec![AtomId::claim(7), AtomId::claim(8)]
            })),
            vec!["claim-0001", "claim-0007", "claim-0008"],
        );
    }

    #[test]
    fn referenced_ids_are_empty_without_references_and_exclude_evidence() {
        // An Entity with no participants references nothing — and its
        // `first_appearance` chunk is EVIDENCE, not a referenced atom. The two
        // accessors must not bleed into each other.
        let entity = sample_entity("Justice", "ch001");
        assert!(entity.referenced_atom_ids().is_empty());
        assert_eq!(entity.evidence().len(), 1, "evidence is the other accessor");
    }

    #[test]
    fn stable_key_unit_separator_prevents_field_concatenation_collision() {
        // Without the unit separator, ("Justice", "ch001") and
        // ("Justicec", "h001") would hash the same. Pin that the separator
        // does its job.
        let a = sample_entity("Justice", "ch001");
        let b = sample_entity("Justicec", "h001");
        assert_ne!(a.stable_key("wikipedia"), b.stable_key("wikipedia"));
    }
}
