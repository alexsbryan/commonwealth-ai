// SPDX-License-Identifier: AGPL-3.0-or-later
//! A typed query answered over a loaded graph: the declared ontology's read
//! side, for a caller that holds an [`AtlasGraph`] rather than an atlas dir.
//!
//! The semantics are [`understanding_atlas::atlas_traversal::typed::execute`]'s
//! and only its — `svrn enrich atlas-query --typed` runs the same body over
//! `atoms.json`, so the CLI and chat cannot answer "which atoms of this type"
//! two ways. The executor reads entities, claims and relations as typed atoms
//! (`typed_match::Judge::new`); a graph holds projected records with the whole
//! atom as a JSON payload, so those three kinds are re-parsed from the
//! payloads once per graph, on the first typed query, and kept beside it.

use understanding_atlas::atlas_traversal::engine::AtlasView;
use understanding_atlas::atlas_traversal::typed::execute;
use understanding_vocab::atoms::{AtomEnvelope, Claim, Entity, Relation};

use super::AtlasGraph;
use crate::atoms::AtomType;

pub use understanding_atlas::atlas_traversal::brief::{assemble_brief, Brief};
pub use understanding_atlas::atlas_traversal::engine::TraversalResult;
pub use understanding_atlas::atlas_traversal::typed::{TypedQuery, TypedRow, TypedTable};
pub use understanding_atlas::atlas_traversal::typed_prompt::{
    query_grammar, QueryGrammar, QUERY_SYSTEM,
};

/// The three atom kinds the executor reads, typed.
#[derive(Debug, Default)]
pub struct TypedAtoms {
    entities: Vec<Entity>,
    claims: Vec<Claim>,
    relations: Vec<Relation>,
}

impl TypedAtoms {
    fn from_graph(graph: &AtlasGraph) -> Self {
        let started = std::time::Instant::now();
        let mut out = Self::default();
        let mut unreadable = 0usize;
        for kind in [AtomType::Entity, AtomType::Claim, AtomType::Relation] {
            for view in graph.atoms_of_kind(kind) {
                match view.atom_envelope() {
                    Some(AtomEnvelope::Entity(e)) => out.entities.push(e),
                    Some(AtomEnvelope::Claim(c)) => out.claims.push(c),
                    Some(AtomEnvelope::Relation(r)) => out.relations.push(r),
                    _ => unreadable += 1,
                }
            }
        }
        // An unreadable payload is an atom the answer cannot see: counted and
        // named, never silently dropped.
        if unreadable > 0 {
            tracing::warn!(
                target: "retrieval_audit",
                corpus = %graph.atlas_corpus_id,
                unreadable,
                "typed atoms: payloads that did not parse as their kind are absent from typed answers"
            );
        }
        tracing::debug!(
            target: "retrieval_audit",
            event = "typed_atoms_loaded",
            corpus = %graph.atlas_corpus_id,
            entities = out.entities.len(),
            claims = out.claims.len(),
            relations = out.relations.len(),
            unreadable,
            ms = started.elapsed().as_millis() as u64,
            "typed atoms parsed from the graph's payloads"
        );
        out
    }
}

impl AtlasGraph {
    /// Run a typed query over this atlas. `None` when the corpus declared no
    /// types — the [`Self::ontology`] gate every declared-type path shares.
    pub fn typed_answer(&self, query: &TypedQuery) -> Option<TraversalResult> {
        let vocab = self.ontology()?;
        let atoms = self
            .typed_atoms
            .get_or_init(|| TypedAtoms::from_graph(self));
        Some(execute(
            query,
            AtlasView {
                entities: &atoms.entities,
                events: &[],
                states: &[],
                relations: &atoms.relations,
                claims: &atoms.claims,
                questions: &[],
                configurations: &[],
                edges: &[],
                positions: &[],
                oppositions: &[],
                vocab: Some(vocab),
            },
        ))
    }
}
