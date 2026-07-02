// SPDX-License-Identifier: AGPL-3.0-or-later
//! Topic nodes — the *unit* of cross-corpus alignment.
//!
//! The bridge aligns **topics** (concept/article units), not the
//! per-name `Entity` clusters the rest of the meta-atlas keys on. A
//! [`BridgeTopic`] is the structural fingerprint of one article on one
//! side of the alignment:
//!
//!   - **SEP topic** = one `sep-<slug>` per-article atlas. SEP atlases
//!     are argument-rich (Claims, ArgumentReconstructions, Questions),
//!     so the topic is Argument-dominant by construction.
//!   - **Wikipedia topic** = one `page_id`'s Entity atom(s), gathered
//!     via `doc_to_atoms.json`. Wikipedia is structural-first — every
//!     article is a pure-Inventory Entity.
//!
//! This module is deliberately **synchronous and file-driven**: it
//! reads an atlas's atoms and folds them into a `BridgeTopic` with no
//! embedding and no inference. Embedding population (and the RAPTOR
//! whole-document summary that supersedes `concept_text` on the SEP
//! side) is an *injected* concern handled at candidate-generation time
//! — the RAPTOR-summaries reader lives in `sovereign-tools`, which
//! depends on this crate, so reading it here would be a cyclic dep
//! (same reason the tiered RAPTOR builder is injected via a trait).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::atlas_canonical::lookup_key;
use crate::enrichment::atlas::atoms::AtomType;
use crate::enrichment::atlas::{read_atlas_atoms, AtomEnvelope};
use crate::enrichment::pipeline::atlas::EntityType;
use crate::meta_atlas::classifier::{classify_articulation, classify_by_chunk_preview};
use crate::stream_axes::ArticulationVector;
use crate::types::ScoredChunk;

/// One article on one side of the alignment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeTopic {
    /// The owning corpus — `sep-<slug>` for an SEP article, `wikipedia`
    /// for a Wikipedia page.
    pub corpus_id: String,
    /// Stable per-corpus topic handle — the SEP slug, or the Wikipedia
    /// `page_id`.
    pub topic_id: String,
    /// Reader-facing title — the highest-salience Concept entity's
    /// canonical name (fallback: highest-salience entity, then
    /// `topic_id`).
    pub title: String,
    /// Text embedded for candidate generation. On the SEP side this is
    /// superseded by the RAPTOR whole-document summary when one is
    /// available (injected at Phase 1); the value built here —
    /// `title + description + defining_quote` — is the fallback.
    pub concept_text: String,
    /// Normalised `lookup_key` of every Entity atom (canonical name +
    /// aliases). The "what this article names" set — the substrate of
    /// the SharedEntities alignment signal, and where the demoted
    /// name-cluster meta-atom becomes a *feature*.
    pub entity_keys: BTreeSet<String>,
    /// Names of the article's argument structure — `ArgumentReconstruction`
    /// names + `Position` labels. Empty for Wikipedia (inventory-only).
    /// Fed to the LLM adjudicator so it can judge the relation against
    /// what the SEP article actually argues.
    #[serde(default)]
    pub argument_names: Vec<String>,
    /// Per-`AtomType` counts — the article's shape. SEP articles carry
    /// many Claim/ArgumentReconstruction/Question atoms; Wikipedia
    /// pages are Entity-only. Drives the granularity heuristic.
    pub atom_profile: BTreeMap<AtomType, u64>,
    /// Aggregate Inventory/Argument/Trace articulation over the
    /// article's atoms. SEP → Argument-dominant, Wikipedia → Inventory-
    /// dominant; the complementarity is the "two registers" signal.
    pub articulation: ArticulationVector,
    /// Populated at candidate-generation time (RAPTOR summary embedding
    /// on the SEP side, or `EmbedFn` over `concept_text`). `None` until
    /// then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
}

impl BridgeTopic {
    /// Fold a set of atoms (all belonging to one article) into a topic.
    ///
    /// Pure: no I/O, no inference. `corpus_id` + `topic_id` identify the
    /// article; `atoms` is every atom the article produced.
    pub fn from_atoms(
        corpus_id: impl Into<String>,
        topic_id: impl Into<String>,
        atoms: &[AtomEnvelope],
    ) -> Self {
        let corpus_id = corpus_id.into();
        let topic_id = topic_id.into();

        let mut entity_keys: BTreeSet<String> = BTreeSet::new();
        let mut atom_profile: BTreeMap<AtomType, u64> = BTreeMap::new();
        let (mut inv, mut arg, mut trc, mut n) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        let mut argument_names: Vec<String> = Vec::new();

        // Title candidates, stored owned to sidestep borrow friction.
        let mut best_concept: Option<(f32, String, String, Option<String>)> = None;
        let mut best_any: Option<(f32, String)> = None;

        for env in atoms {
            *atom_profile.entry(atom_type_of(env)).or_insert(0) += 1;

            let v = classify_articulation(env, preview_of(env));
            inv += v.inventory;
            arg += v.argument;
            trc += v.trace;
            n += 1.0;

            match env {
                AtomEnvelope::ArgumentReconstruction(a) => argument_names.push(a.name.clone()),
                AtomEnvelope::Position(p) => argument_names.push(p.canonical_name.clone()),
                _ => {}
            }

            if let AtomEnvelope::Entity(e) = env {
                let k = lookup_key(&e.canonical_name);
                if !k.is_empty() {
                    entity_keys.insert(k);
                }
                for a in &e.aliases {
                    let ak = lookup_key(a);
                    if !ak.is_empty() {
                        entity_keys.insert(ak);
                    }
                }
                if matches!(e.entity_type, EntityType::Concept)
                    && best_concept.as_ref().is_none_or(|(s, ..)| e.salience > *s)
                {
                    best_concept = Some((
                        e.salience,
                        e.canonical_name.clone(),
                        e.description.clone(),
                        e.defining_quote.clone(),
                    ));
                }
                if best_any.as_ref().is_none_or(|(s, _)| e.salience > *s) {
                    best_any = Some((e.salience, e.canonical_name.clone()));
                }
            }
        }

        let (title, concept_text) = match best_concept {
            Some((_, name, desc, quote)) => {
                let mut ct = if desc.is_empty() {
                    name.clone()
                } else {
                    format!("{name}\n{desc}")
                };
                if let Some(q) = quote.filter(|q| !q.is_empty()) {
                    ct.push('\n');
                    ct.push_str(&q);
                }
                (name, ct)
            }
            None => {
                let name = best_any.map(|(_, n)| n).unwrap_or_else(|| topic_id.clone());
                (name.clone(), name)
            }
        };

        let articulation = if n > 0.0 {
            ArticulationVector::new(inv / n, arg / n, trc / n)
        } else {
            ArticulationVector::balanced()
        };

        Self {
            corpus_id,
            topic_id,
            title,
            concept_text,
            entity_keys,
            argument_names,
            atom_profile,
            articulation,
            embedding: None,
        }
    }

    /// Total atoms across all types — the cheap "depth" proxy used by
    /// the granularity heuristic (an SEP article with 90 atoms is a
    /// richer treatment than a 2-atom Wikipedia stub).
    pub fn atom_count(&self) -> u64 {
        self.atom_profile.values().copied().sum()
    }

    /// Count of argument-bearing atoms (Claim + ArgumentReconstruction
    /// + Position + Opposition + Question). The SEP side's signature.
    pub fn argument_atom_count(&self) -> u64 {
        use AtomType::*;
        [
            Claim,
            ArgumentReconstruction,
            Position,
            Opposition,
            Question,
        ]
        .iter()
        .filter_map(|t| self.atom_profile.get(t))
        .copied()
        .sum()
    }
}

/// Build a topic by reading a per-article atlas dir — the *driver*-side
/// build. `corpus_id` is the atlas's owning corpus (e.g. `sep-abduction`),
/// `topic_id` the stable per-corpus handle (e.g. the slug). Returns
/// `Ok(None)` when the atlas has no atoms (nothing to align).
pub fn topic_from_atlas(
    corpus_id: &str,
    topic_id: &str,
    atlas_dir: &Path,
) -> std::io::Result<Option<BridgeTopic>> {
    let atoms = read_atlas_atoms(atlas_dir)?;
    if atoms.atoms.is_empty() {
        return Ok(None);
    }
    Ok(Some(BridgeTopic::from_atoms(
        corpus_id,
        topic_id,
        &atoms.atoms,
    )))
}

/// Build a topic directly from an ANN search hit against `corpus_id` —
/// the *candidate*-side build. Avoids loading a large atlas to gather a
/// page's atoms: the lead chunk text is a serviceable gloss, the title
/// is the sole entity key, and articulation is read from the lead text
/// via the chunk-preview classifier. Returns `None` for a titleless hit.
pub fn topic_from_chunk(corpus_id: &str, hit: &ScoredChunk) -> Option<BridgeTopic> {
    let title = hit.title.clone()?;
    if title.trim().is_empty() {
        return None;
    }
    let topic_id = hit.source_doc_id.clone().unwrap_or_else(|| title.clone());
    let mut entity_keys = BTreeSet::new();
    let key = lookup_key(&title);
    if !key.is_empty() {
        entity_keys.insert(key);
    }
    let mut atom_profile = BTreeMap::new();
    atom_profile.insert(AtomType::Entity, 1);
    Some(BridgeTopic {
        corpus_id: corpus_id.to_string(),
        topic_id,
        title,
        concept_text: hit.content.clone(),
        entity_keys,
        argument_names: Vec::new(),
        atom_profile,
        articulation: classify_by_chunk_preview(&hit.content),
        embedding: None,
    })
}

/// The chunk-preview the articulation classifier reads. For Entities
/// the corpus-side gloss (`description`) carries the lead-sentence
/// shape the classifier keys on; other atom variants don't need a
/// preview (their type alone determines articulation).
fn preview_of(env: &AtomEnvelope) -> &str {
    match env {
        AtomEnvelope::Entity(e) => &e.description,
        _ => "",
    }
}

/// Map an atom envelope to its [`AtomType`]. Mirrors the match in
/// `enrichment::atlas::summary` — there is no `AtomEnvelope::atom_type`
/// accessor to reuse.
fn atom_type_of(env: &AtomEnvelope) -> AtomType {
    match env {
        AtomEnvelope::Entity(_) => AtomType::Entity,
        AtomEnvelope::Event(_) => AtomType::Event,
        AtomEnvelope::State(_) => AtomType::State,
        AtomEnvelope::Relation(_) => AtomType::Relation,
        AtomEnvelope::Claim(_) => AtomType::Claim,
        AtomEnvelope::Question(_) => AtomType::Question,
        AtomEnvelope::Configuration(_) => AtomType::Configuration,
        AtomEnvelope::ArgumentReconstruction(_) => AtomType::ArgumentReconstruction,
        AtomEnvelope::Position(_) => AtomType::Position,
        AtomEnvelope::Opposition(_) => AtomType::Opposition,
        AtomEnvelope::Asset(_) => AtomType::Asset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::{
        ArgumentReconstruction, Claim, Entity, Question, ResolutionStatus,
    };
    use crate::enrichment::atlas::{AtomId, ChunkRef, SectionPosition};
    use crate::enrichment::pipeline::atlas::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EpistemicStatus, QuestionType,
    };
    use crate::stream_axes::Articulation;

    fn entity(name: &str, et: EntityType, salience: f32, quote: Option<&str>) -> AtomEnvelope {
        AtomEnvelope::Entity(Entity {
            id: AtomId::entity(1),
            canonical_name: name.into(),
            aliases: Vec::new(),
            entity_type: et,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: format!("a gloss for {name}"),
            defining_quote: quote.map(String::from),
            salience,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        })
    }

    fn claim(act: DiscourseAct) -> AtomEnvelope {
        AtomEnvelope::Claim(Claim {
            id: AtomId::claim(1),
            content: "some claim".into(),
            discourse_act: act,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: Vec::new(),
            quotable_excerpt: None,
            attributed_to: None,
            confidence: None,
            anchor: None,
            claim_kind: None,
            concession_outcome: None,
            evidence_kind: None,
            enrichment_depth: EnrichmentDepth::Extracted,
        })
    }

    fn argument() -> AtomEnvelope {
        AtomEnvelope::ArgumentReconstruction(ArgumentReconstruction {
            id: AtomId::argument_reconstruction(1),
            name: "Some Argument".into(),
            proponent: None,
            premises: vec!["p1".into()],
            conclusion: "c".into(),
            objections: Vec::new(),
            evidence: Vec::new(),
            section_position: SectionPosition::section("sec_0001"),
            enrichment_depth: EnrichmentDepth::Extracted,
        })
    }

    fn question() -> AtomEnvelope {
        AtomEnvelope::Question(Question {
            id: AtomId::question(1),
            content: "is it so?".into(),
            question_type: QuestionType::Thematic,
            addressed_by: Vec::new(),
            raised_at: Vec::new(),
            resolution_status: ResolutionStatus::Open,
            enrichment_depth: EnrichmentDepth::Extracted,
        })
    }

    #[test]
    fn sep_shaped_topic_is_argument_dominant_and_titled_by_concept() {
        // A philosophy article: one Concept entity + argument atoms.
        let atoms = vec![
            entity(
                "Abduction",
                EntityType::Concept,
                0.9,
                Some("Abduction is IBE."),
            ),
            entity("Charles Peirce", EntityType::Person, 0.6, None),
            claim(DiscourseAct::Argue),
            claim(DiscourseAct::Assert),
            argument(),
            question(),
        ];
        let t = BridgeTopic::from_atoms("sep-abduction", "abduction", &atoms);
        assert_eq!(t.title, "Abduction");
        assert!(t.concept_text.contains("Abduction is IBE."));
        assert_eq!(t.articulation.dominant(), Articulation::Argument);
        // entity_keys carries both named entities, normalised.
        assert!(t.entity_keys.contains("abduction"));
        assert!(t.entity_keys.contains("charles peirce"));
        assert_eq!(t.atom_count(), 6);
        assert_eq!(t.argument_atom_count(), 4); // 2 claims + 1 arg + 1 question
    }

    #[test]
    fn wp_shaped_topic_is_inventory_dominant() {
        // A structural-first wiki page: one Other("article") entity
        // whose gloss opens like a Wikipedia lead.
        let mut e = entity(
            "Abductive reasoning",
            EntityType::Other("article".into()),
            0.5,
            None,
        );
        if let AtomEnvelope::Entity(ent) = &mut e {
            ent.description = "'''Abductive reasoning''' is a form of logical inference.".into();
        }
        let t = BridgeTopic::from_atoms("wikipedia", "12345", std::slice::from_ref(&e));
        assert_eq!(t.corpus_id, "wikipedia");
        assert_eq!(t.topic_id, "12345");
        assert_eq!(t.title, "Abductive reasoning");
        assert_eq!(t.articulation.dominant(), Articulation::Inventory);
        assert!(t.entity_keys.contains("abductive reasoning"));
    }

    #[test]
    fn from_atoms_empty_titles_by_topic_id() {
        let t = BridgeTopic::from_atoms("c", "the-id", &[]);
        assert_eq!(t.title, "the-id");
        assert_eq!(t.atom_count(), 0);
    }

    #[test]
    fn wp_topic_from_chunk_builds_inventory_topic_from_hit() {
        let hit = ScoredChunk {
            content: "'''Abductive reasoning''' is a form of logical inference.".into(),
            title: Some("Abductive reasoning".into()),
            url: None,
            corpus_id: "wikipedia".into(),
            score: 0.9,
            metadata: std::collections::HashMap::new(),
            chunk_id: Some(42),
            source_doc_id: Some("12345".into()),
            vector_distance: Some(0.2),
        };
        let t = topic_from_chunk("wikipedia", &hit).unwrap();
        assert_eq!(t.corpus_id, "wikipedia");
        assert_eq!(t.topic_id, "12345");
        assert_eq!(t.title, "Abductive reasoning");
        assert!(t.entity_keys.contains("abductive reasoning"));
        assert_eq!(
            t.articulation.dominant(),
            crate::stream_axes::Articulation::Inventory
        );

        // A titleless hit yields no topic.
        let mut bad = hit.clone();
        bad.title = None;
        assert!(topic_from_chunk("wikipedia", &bad).is_none());
    }

    #[test]
    fn title_falls_back_to_topic_id_without_entities() {
        // Only a claim, no entity — title can't come from a Concept.
        let t = BridgeTopic::from_atoms("sep-x", "x-slug", &[claim(DiscourseAct::Argue)]);
        assert_eq!(t.title, "x-slug");
    }
}
