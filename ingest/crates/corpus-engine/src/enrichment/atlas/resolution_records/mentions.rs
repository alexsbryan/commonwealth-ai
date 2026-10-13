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

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;
use tracing::debug;

use super::super::atoms::{AtomId, Entity};
use super::super::resolution_sources::{Participants, SOURCE_EXTRACTOR_ID};
use super::super::resolve_records::propose::FieldNamed;
use super::super::resolve_records::Resolver;
use super::BuildAtoms;
use crate::enrichment::ontology::{OntologyTypeDecl, TypeIndex};
use crate::enrichment::reconciliation::identity_signals::fold_identity_value;

/// How many of the records pointing at a seeded record its card names, per
/// pointing type and attribute. A cost knob: it bounds the prompt.
const POINTING_SHOWN: usize = 3;

/// What seeding gave a type: each seeded record's atom (record id -> atom id),
/// per document the seeded records its fields name, and each seeded record's
/// card as one text (`Record::shown`), in seeding order, for `Nearest`.
#[derive(Debug, Default)]
pub(super) struct Seeded {
    pub(super) atoms: HashMap<String, AtomId>,
    named: HashMap<String, Vec<(String, String)>>,
    pub(super) cards: Vec<(String, String)>,
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
        let shown = card(e, &identity, atoms, index);
        debug!(r#type = %t.name, record = %id, name = %e.canonical_name, keys = ?keys, shown = ?shown, "atlas/resolve: a source record seeded");
        out.cards.push((id.clone(), shown.join("; ")));
        resolver.show(&id, shown);
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

/// What a seeded record is shown as, as written (`Record::shown`): each
/// identity value as its source read it, its name when that is none of them,
/// and the records whose refs point at it, by type and attribute, each named
/// with its own identity values, so a reader recognises the record by what a
/// document would write of it (a company by the people whose employer it is).
fn card(
    e: &Entity,
    identity: &[String],
    atoms: &BuildAtoms<'_>,
    index: &TypeIndex<'_>,
) -> Vec<String> {
    let values: Vec<(String, String)> = identity
        .iter()
        .filter_map(|k| Some((k.clone(), e.attributes.get(k)?.as_str()?.to_string())))
        .collect();
    let mut out: Vec<String> = values.iter().map(|(k, v)| format!("{k}: {v}")).collect();
    if !values.iter().any(|(_, v)| *v == e.canonical_name) && !e.canonical_name.is_empty() {
        out.push(format!("name: {}", e.canonical_name));
    }
    let id = e.id.as_str();
    let mut pointing: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for other in atoms.entities.iter().filter(|o| o.id.as_str() != id) {
        for (attr, v) in &other.attributes {
            let points = match v {
                Value::String(s) => s == id,
                Value::Array(a) => a.iter().any(|x| x.as_str() == Some(id)),
                _ => false,
            };
            if !points {
                continue;
            }
            let ty = other.entity_type.as_str_repr().to_string();
            let own: Vec<&str> = index
                .effective_identity(&ty)
                .iter()
                .filter_map(|k| other.attributes.get(k)?.as_str())
                .collect();
            let named = if own.is_empty() {
                other.canonical_name.clone()
            } else {
                format!("{} ({})", other.canonical_name, own.join(", "))
            };
            pointing.entry((ty, attr.clone())).or_default().push(named);
        }
    }
    for ((ty, attr), mut names) in pointing {
        names.sort();
        names.dedup();
        let more = names.len().saturating_sub(POINTING_SHOWN);
        names.truncate(POINTING_SHOWN);
        let tail = if more > 0 { format!(", and {more} more") } else { String::new() };
        out.push(format!("{ty} whose {attr} it is: {}{tail}", names.join(", ")));
    }
    out
}
