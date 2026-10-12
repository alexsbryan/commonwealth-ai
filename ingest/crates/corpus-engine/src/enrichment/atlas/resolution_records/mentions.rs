// SPDX-License-Identifier: AGPL-3.0-or-later
//! The records RESOLVE starts a mentioned type with (`decides_mentions`): a
//! metadata source names only who took part in a document, so the text names
//! particulars it never read. Its records (`resolution_sources`, extractor
//! `document_fields`) are seeded under their own atom ids, holding their
//! source's keys, so a mention links to one or opens a record of its own, and
//! the source's atoms and every reference to them survive (ARCH 8: one
//! particular, one id). Each is offered to the mentions of the documents whose
//! fields name it (`propose::FieldNamed`): no other proposer can offer a record
//! no statement has reached.

use std::collections::HashMap;

use tracing::debug;

use super::super::atoms::AtomId;
use super::super::resolution_sources::{Participants, SOURCE_EXTRACTOR_ID};
use super::super::resolve_records::propose::FieldNamed;
use super::super::resolve_records::Resolver;
use super::BuildAtoms;
use crate::enrichment::ontology::{OntologyTypeDecl, TypeIndex};
use crate::enrichment::reconciliation::identity_signals::fold_identity_value;

/// What seeding gave a type: each seeded record's atom (record id -> atom id)
/// and, per document, the seeded records its fields name.
#[derive(Debug, Default)]
pub(super) struct Seeded {
    pub(super) atoms: HashMap<String, AtomId>,
    named: HashMap<String, Vec<(String, String)>>,
}

impl Seeded {
    /// The proposer offering each document's field-named records.
    pub(super) fn named(&self) -> FieldNamed {
        FieldNamed::new(self.named.clone())
    }
}

/// Seed `resolver` with `t`'s source records: one record per atom the source
/// projected, its id the atom's, its keys the atom's identity values folded as
/// every statement key is (`fields::declared_keys`).
pub(super) fn seed(
    resolver: &mut Resolver,
    atoms: &BuildAtoms<'_>,
    t: &OntologyTypeDecl,
    index: &TypeIndex<'_>,
    participants: &Participants,
) -> Seeded {
    let identity = index.effective_identity(&t.name);
    let mut out = Seeded::default();
    for e in atoms
        .entities
        .iter()
        .filter(|e| e.entity_type.as_str_repr() == t.name)
        .filter(|e| e.provenance.extractor_id == SOURCE_EXTRACTOR_ID)
    {
        let keys: Vec<(String, String)> = identity
            .iter()
            .filter_map(|k| {
                let v = e.attributes.get(k)?.as_str()?;
                Some((k.clone(), fold_identity_value(v)?))
            })
            .collect();
        let id = e.id.as_str().to_string();
        resolver.seed(&id, &keys);
        debug!(r#type = %t.name, record = %id, name = %e.canonical_name, keys = ?keys, "atlas/resolve: a source record seeded");
        out.atoms.insert(id, e.id.clone());
    }
    for (doc, named) in participants {
        let mine: Vec<(String, String)> = named
            .iter()
            .filter(|(_, id)| out.atoms.contains_key(id.as_str()))
            .map(|(field, id)| (id.as_str().to_string(), field.clone()))
            .collect();
        if !mine.is_empty() {
            out.named.insert(doc.clone(), mine);
        }
    }
    out
}
