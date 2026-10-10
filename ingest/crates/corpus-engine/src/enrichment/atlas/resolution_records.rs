// SPDX-License-Identifier: AGPL-3.0-or-later
//! RESOLVE in the atlas build (svrn/docs/specs/ONTOLOGY_METHOD.md §Identity,
//! Ring 3). A type RESOLVE decides ([`decides`]) has its particulars from
//! RESOLVE alone. Its statements are the claims of every kind declaring
//! `subject = <type>`, each the span its anchor marks in the one document it
//! lands in (`resolution_documents::locate`), documents in clock order. Its
//! atoms are RESOLVE's records, each id hashed from the statement that opened
//! it (ARCH 8: identity from essence), and each claim's subject is its
//! statement's record, or none where RESOLVE refused the statement.
//!
//! The atoms 3a made of the type from Phase-1 sketches, merged by name, are
//! retired with every reference to them, so no second decider survives. A
//! reference to a retired atom is dropped and recorded, never repointed: a
//! mention that is not a statement has no decided particular to point at
//! (ARCH 6).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::Serialize;
use serde_json::{Map, Value};
use tracing::{debug, info};

use super::atoms::{
    ArgumentReconstruction, AtomId, ChunkRef, Claim, Entity, Event, Opposition, Position, Relation,
    SectionPosition, SignalKind, SignalProvenance, State,
};
use super::edges::Edge;
use super::resolution::Trajectory;
use super::resolution_documents::{
    failure, fold_ws, locate, read_stamp, SectionDocuments, SourceDocument,
};
use super::resolve_records::propose::{
    Proposers, SimilarDocuments, MAX_CANDIDATES, MIN_SIMILARITY, NEIGHBOURS,
};
use super::resolve_records::{
    resolve_in_clock_order, Answerer, Criterion, Document, DocumentResolution, ProposalRule,
    Resolver, Statement,
};
use crate::enrichment::ontology::{
    DocumentStamp, OntologyPolicies, OntologyTypeDecl, TypeIndex, TypeKind,
};
use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType, EventType};
use crate::enrichment::pipeline::document_read::{
    LOCAL_REF_ATTRIBUTE, SOURCE_DOCUMENT_ATTRIBUTE, SUBJECT_FIELDS_ATTRIBUTE,
};
use crate::enrichment::pipeline::types::{PhaseFailure, PhaseFailureKind};

#[path = "resolution_records/local_subjects.rs"]
mod local_subjects;
#[path = "resolution_records/retire.rs"]
mod retire;
use local_subjects::{Assigned, Spot};
use retire::retire;

/// The extractor id on a record's atom.
const EXTRACTOR_ID: &str = "atlas/resolve";

/// Whether RESOLVE decides `type_name`'s identity in the atlas build: a type
/// of a kind that is identified again (an entity or an event; PRIMITIVES §0:
/// events are full subjects of claims) that declares an `identity_criterion`
/// and no metadata `source`. The one test: 3b leaves a claim's subject of such
/// a type to RESOLVE, and [`resolve_declared_types`] decides it.
pub fn decides(index: &TypeIndex<'_>, type_name: &str) -> bool {
    index.get(type_name).is_some_and(|t| {
        matches!(t.kind, TypeKind::Entity | TypeKind::Event)
            && t.identity_criterion.is_some()
            && t.source.is_none()
    })
}

/// The types RESOLVE decides ([`decides`]) that no declared claim kind names as
/// its `subject`: RESOLVE would have no statement of them, and their Phase-1
/// atoms would be decided by 3a's merge instead. The build refuses such a
/// declaration (`enrich extract`); `recipe validate` names it.
pub fn types_without_statements(policies: &OntologyPolicies) -> Vec<String> {
    let index = TypeIndex::from_policies(policies);
    policies
        .shape
        .types
        .iter()
        .filter(|t| decides(&index, &t.name))
        .filter(|t| {
            !policies
                .shape
                .types
                .iter()
                .any(|c| c.kind == TypeKind::Claim && c.subject.as_deref() == Some(t.name.as_str()))
        })
        .map(|t| t.name.clone())
        .collect()
}

/// Every atom vector of a build that can name an entity. Retiring one
/// touches each; questions name claims only, and no claim is retired.
pub struct BuildAtoms<'a> {
    pub entities: &'a mut Vec<Entity>,
    pub events: &'a mut Vec<Event>,
    pub states: &'a mut Vec<State>,
    pub relations: &'a mut Vec<Relation>,
    pub claims: &'a mut Vec<Claim>,
    pub argument_reconstructions: &'a mut Vec<ArgumentReconstruction>,
    pub positions: &'a mut Vec<Position>,
    pub oppositions: &'a mut Vec<Opposition>,
    pub edges: &'a mut Vec<Edge>,
    pub trajectories: &'a mut BTreeMap<String, Trajectory>,
}

/// What RESOLVE did for one type.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RecordsReport {
    pub type_name: String,
    /// Claims of the kinds declaring this type their subject.
    pub claims: usize,
    /// The distinct spans those claims mark: RESOLVE's statements.
    pub statements: usize,
    /// Claims with no document, or no anchor in it: no statement, no subject.
    pub unplaced: usize,
    pub documents: usize,
    /// Documents with no date stamp, resolved after every dated one.
    pub undated: usize,
    /// Statements by how RESOLVE decided them (`Outcome::label`).
    pub outcomes: BTreeMap<&'static str, usize>,
    pub records: usize,
    pub calls: u32,
    /// The type's atoms 3a made from Phase-1 sketches, retired.
    pub retired: usize,
    /// References to them dropped, by what held each.
    pub dropped: BTreeMap<&'static str, usize>,
    /// Each source's weight as estimated on this corpus and what it carried,
    /// one line each (`Resolver::sources_summary`, D2).
    pub sources: Vec<String>,
}

impl RecordsReport {
    /// One line for the resolve step's output.
    pub fn summary(&self) -> String {
        let fold = |m: &BTreeMap<&'static str, usize>| {
            m.iter()
                .map(|(k, n)| format!("{k} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            "RESOLVE `{}`: {} record(s) from {} statement(s) of {} claim(s) in {} document(s) \
             ({} unplaced, {} undated), {} call(s); decided [{}]; {} Phase-1 atom(s) retired, \
             references dropped [{}]",
            self.type_name,
            self.records,
            self.statements,
            self.claims,
            self.documents,
            self.unplaced,
            self.undated,
            self.calls,
            fold(&self.outcomes),
            self.retired,
            fold(&self.dropped),
        ) + &self
            .sources
            .iter()
            .map(|l| format!("\n    source {l}"))
            .collect::<String>()
    }
}

/// One document's statements, as RESOLVE is handed them.
struct Placed<'d> {
    doc: &'d SourceDocument,
    body: String,
    stamps: Vec<(DocumentStamp, String)>,
    statements: Vec<Statement>,
}

/// Decide the identity of every type RESOLVE decides, in declaration order.
/// `on_document` sees each document's resolution as it is made, so the caller
/// can keep the decisions beside the atlas.
pub async fn resolve_declared_types(
    atoms: &mut BuildAtoms<'_>,
    documents: &SectionDocuments,
    policies: &OntologyPolicies,
    corpus_id: &str,
    answerer: Answerer<'_>,
    on_document: &mut (dyn FnMut(&str, &DocumentResolution) + Send),
) -> (Vec<RecordsReport>, Vec<PhaseFailure>) {
    let index = TypeIndex::from_policies(policies);
    let (mut reports, mut failures) = (Vec::new(), Vec::new());
    for t in policies.shape.types.iter() {
        if t.identity_criterion.is_none() {
            continue;
        }
        if !decides(&index, &t.name) {
            let why = if t.source.is_some() {
                "it declares a metadata source".to_string()
            } else {
                format!("it is a {:?} type", t.kind)
            };
            info!(r#type = %t.name, %why, "atlas/resolve: an identity criterion RESOLVE does not decide in the atlas build");
            failures.push(failure(
                format!("type:{}", t.name),
                PhaseFailureKind::Other,
                format!(
                    "`{}` declares an identity_criterion, but {why}: RESOLVE decides only entity \
                     and event types with no source in the atlas build, so its atoms are decided \
                     as before",
                    t.name
                ),
            ));
            continue;
        }
        let report = resolve_type(
            atoms,
            t,
            &index,
            documents,
            policies,
            corpus_id,
            answerer,
            on_document,
            &mut failures,
        )
        .await;
        info!(r#type = %t.name, records = report.records, statements = report.statements, unplaced = report.unplaced, retired = report.retired, calls = report.calls, "atlas/resolve: type decided");
        reports.push(report);
    }
    for claim in atoms.claims.iter_mut() {
        claim.attributes.remove(LOCAL_REF_ATTRIBUTE);
        claim.attributes.remove(SOURCE_DOCUMENT_ATTRIBUTE);
        claim.attributes.remove(SUBJECT_FIELDS_ATTRIBUTE);
    }
    (reports, failures)
}

#[allow(clippy::too_many_arguments)]
async fn resolve_type(
    atoms: &mut BuildAtoms<'_>,
    t: &OntologyTypeDecl,
    index: &TypeIndex<'_>,
    documents: &SectionDocuments,
    policies: &OntologyPolicies,
    corpus_id: &str,
    answerer: Answerer<'_>,
    on_document: &mut (dyn FnMut(&str, &DocumentResolution) + Send),
    failures: &mut Vec<PhaseFailure>,
) -> RecordsReport {
    let mut report = RecordsReport {
        type_name: t.name.clone(),
        ..Default::default()
    };
    let kind = t.kind;
    let kinds: BTreeSet<&str> = policies
        .shape
        .types
        .iter()
        .filter(|c| c.kind == TypeKind::Claim && c.subject.as_deref() == Some(t.name.as_str()))
        .map(|c| c.name.as_str())
        .collect();
    let of_kinds = |c: &Claim| c.claim_kind.as_deref().is_some_and(|k| kinds.contains(k));
    // Reader-local references and supplied values are transient handoff data;
    // the qualified read itself remains in the Phase-1 cache.
    let mut read_document_of = HashMap::<usize, String>::new();
    let mut read_local_ref_of = HashMap::<usize, String>::new();
    let mut read_subject_fields_of = HashMap::<usize, Map<String, Value>>::new();
    for (i, claim) in atoms.claims.iter_mut().enumerate() {
        if !of_kinds(claim) {
            continue;
        }
        claim.subject = None;
        if let Some(Value::String(document)) = claim.attributes.remove(SOURCE_DOCUMENT_ATTRIBUTE) {
            for evidence in &mut claim.evidence {
                evidence.source_doc_id = Some(document.clone());
            }
            read_document_of.insert(i, document);
        }
        if let Some(Value::String(local_ref)) = claim.attributes.remove(LOCAL_REF_ATTRIBUTE) {
            read_local_ref_of.insert(i, local_ref);
        }
        if let Some(Value::Object(fields)) = claim.attributes.remove(SUBJECT_FIELDS_ATTRIBUTE) {
            read_subject_fields_of.insert(i, fields);
        }
    }

    // One decider: the Phase-1 atoms of the type go first, whatever follows,
    // in either kind a sketch may have named it (the reader's compatibility
    // projection names an event type's subjects as entities).
    let retired: BTreeSet<AtomId> = atoms
        .entities
        .iter()
        .filter(|e| e.entity_type.as_str_repr() == t.name)
        .map(|e| e.id.clone())
        .chain(
            atoms
                .events
                .iter()
                .filter(|e| e.event_type.as_str_repr() == t.name)
                .map(|e| e.id.clone()),
        )
        .collect();
    report.retired = retired.len();
    report.dropped = retire(atoms, &retired, &t.name, failures);

    let criterion = match Criterion::of(t, index.effective_identity(&t.name).to_vec()) {
        Ok(c) => c,
        Err(why) => {
            failures.push(failure(
                format!("type:{}", t.name),
                PhaseFailureKind::Other,
                why,
            ));
            return report;
        }
    };
    let declared: Vec<(DocumentStamp, &str)> = policies
        .change
        .document
        .as_ref()
        .map(|d| d.declared().collect())
        .unwrap_or_default();
    let declares_thread = policies
        .change
        .document
        .as_ref()
        .is_some_and(|d| d.thread.is_some());

    // Statements: each claim's anchor span in its one document. Claims that
    // mark one span share its statement, and so do claims the reader gave one
    // local subject reference in that document (`local_subjects`).
    let mut placed: Vec<Placed<'_>> = Vec::new();
    let mut at: HashMap<&str, usize> = HashMap::new();
    let mut conflicting_keys: HashSet<(String, String)> = HashSet::new();
    let mut statement_of: Vec<(usize, String)> = Vec::new();
    let mut section_of: HashMap<String, String> = HashMap::new();
    let mut spots: Vec<Spot> = Vec::new();
    let mut spot_claims: Vec<(usize, Option<String>)> = Vec::new();
    for (i, claim) in atoms.claims.iter().enumerate() {
        let Some(kind) = claim.claim_kind.as_deref().filter(|k| kinds.contains(k)) else {
            continue;
        };
        report.claims += 1;
        let locate_accountable = || {
            let document_id = read_document_of.get(&i).ok_or_else(|| {
                "accountable document id is absent from the claim handoff".to_string()
            })?;
            let section = claim
                .evidence
                .first()
                .map(|evidence| evidence.chunk_id.as_str())
                .ok_or_else(|| "accountable claim carries no section evidence".to_string())?;
            let doc = documents
                .document_for_section(section, document_id)
                .ok_or_else(|| {
                    format!(
                        "accountable document `{document_id}` is not present in evidence section `{section}`"
                    )
                })?;
            let anchor = claim
                .anchor
                .as_deref()
                .or_else(|| {
                    claim
                        .evidence
                        .iter()
                        .find_map(|evidence| evidence.passage_preview.as_deref())
                })
                .map(fold_ws)
                .filter(|anchor| !anchor.is_empty())
                .ok_or_else(|| "the claim carries no anchor".to_string())?;
            if !doc.body().contains(&anchor) {
                return Err(format!(
                    "anchor {anchor:?} is not in explicitly named document `{document_id}`"
                ));
            }
            Ok((doc, anchor))
        };
        let spot = if read_document_of.contains_key(&i) {
            locate_accountable()
        } else {
            locate(claim, documents).and_then(|doc| {
                let anchor = claim
                    .anchor
                    .as_deref()
                    .or_else(|| {
                        claim
                            .evidence
                            .iter()
                            .find_map(|e| e.passage_preview.as_deref())
                    })
                    .map(fold_ws)
                    .filter(|a| !a.is_empty())
                    .ok_or_else(|| "the claim carries no anchor".to_string())?;
                Ok((doc, anchor))
            })
        };
        let (doc, anchor) = match spot {
            Ok(s) => s,
            Err(why) => {
                unplaced(&mut report, failures, claim, kind, &t.name, why);
                continue;
            }
        };
        let k = *at.entry(doc.key.as_str()).or_insert_with(|| {
            let stamps = declared
                .iter()
                .filter_map(|(stamp, field)| match read_stamp(&doc.fields, *stamp, field) {
                    Ok(v) => Some((*stamp, v)),
                    Err(why) => {
                        debug!(document = %doc.key, field = stamp.attr(), %why, "atlas/resolve: stamp unread");
                        None
                    }
                })
                .collect();
            placed.push(Placed {
                doc,
                body: doc.body(),
                stamps,
                statements: Vec::new(),
            });
            placed.len() - 1
        });
        let Some(start) = placed[k].body.find(&anchor) else {
            let why = format!("anchor {anchor:?} is not in document `{}`", doc.key);
            unplaced(&mut report, failures, claim, kind, &t.name, why);
            continue;
        };
        let end = start + anchor.len();
        let mut keys = BTreeMap::new();
        if let Some(fields) = read_subject_fields_of.get(&i) {
            for key in criterion.keys.iter().chain(t.identity_necessary.iter()) {
                let value = fields.get(key).and_then(|value| match value {
                    Value::String(text) => Some(text.clone()),
                    Value::Number(number) => Some(number.to_string()),
                    _ => None,
                });
                if let Some(value) = value {
                    keys.insert(key.clone(), value);
                }
            }
        }
        spots.push(Spot {
            placed: k,
            start,
            end,
            local_ref: read_local_ref_of.get(&i).cloned(),
            keys,
        });
        spot_claims.push((i, claim.evidence.first().map(|e| e.chunk_id.clone())));
    }
    let document_of: Vec<&str> = placed.iter().map(|p| p.doc.key.as_str()).collect();
    let assigned = local_subjects::assign(&document_of, &spots, failures);
    for ((spot, Assigned { id, start, end }), (i, section)) in
        spots.into_iter().zip(assigned).zip(spot_claims)
    {
        let (k, keys) = (spot.placed, spot.keys);
        let document = placed[k].doc;
        if !placed[k].statements.iter().any(|s| s.id == id) {
            placed[k].statements.push(Statement {
                id: id.clone(),
                start,
                end,
                keys,
            });
        } else if let Some(statement) = placed[k].statements.iter_mut().find(|s| s.id == id) {
            for (key, value) in keys {
                let conflict = (id.clone(), key.clone());
                if conflicting_keys.contains(&conflict) {
                    continue;
                }
                match statement.keys.get(&key) {
                    Some(previous) if previous != &value => {
                        statement.keys.remove(&key);
                        conflicting_keys.insert(conflict);
                        failures.push(failure(
                            format!("document:{}", document.key),
                            PhaseFailureKind::Other,
                            format!(
                                "accountable claims for local subject `{}` conflict on identity field `{key}`; withheld from RESOLVE",
                                spot.local_ref.as_deref().unwrap_or("?")
                            ),
                        ));
                    }
                    _ => {
                        statement.keys.entry(key).or_insert(value);
                    }
                }
            }
        }
        if let Some(section) = section {
            section_of.entry(id.clone()).or_insert(section);
        }
        statement_of.push((i, id));
    }
    placed.retain(|p| !p.statements.is_empty());
    for p in placed.iter_mut() {
        p.statements.sort_by_key(|s| (s.start, s.end));
    }
    let undated = |p: &Placed<'_>| !p.stamps.iter().any(|(s, _)| *s == DocumentStamp::Date);
    report.documents = placed.len();
    report.undated = placed.iter().filter(|p| undated(p)).count();
    report.statements = placed.iter().map(|p| p.statements.len()).sum();

    let mut resolver = Resolver::with_rule(ProposalRule::default());
    let mut proposer = Proposers::new(
        declares_thread,
        SimilarDocuments::new(NEIGHBOURS, MAX_CANDIDATES, MIN_SIMILARITY),
    );
    let mut record_of: HashMap<String, String> = HashMap::new();
    let mut refused: HashMap<String, &'static str> = HashMap::new();
    let mut documents: Vec<(Document<'_>, &[Statement])> = placed
        .iter()
        .map(|p| {
            (
                Document {
                    id: &p.doc.key,
                    title: p.doc.title.as_deref(),
                    body: &p.body,
                    stamps: &p.stamps,
                },
                p.statements.as_slice(),
            )
        })
        .collect();
    // The clock orders them (`resolve_records::clock`), one loop for both drivers.
    resolve_in_clock_order(
        &criterion,
        &mut documents,
        &mut resolver,
        &mut proposer,
        answerer,
        &mut |_, _, r| {
            on_document(&t.name, r);
            report.calls += r.calls;
            for o in &r.outcomes {
                *report.outcomes.entry(o.outcome.label()).or_default() += 1;
                match o.outcome.record() {
                    Some(rec) => {
                        record_of.insert(o.statement.clone(), rec.to_string());
                    }
                    None => {
                        refused.insert(o.statement.clone(), o.outcome.label());
                    }
                }
            }
        },
    )
    .await;

    report.sources = resolver.sources_summary();
    for line in &report.sources {
        info!(r#type = %t.name, source = %line, "atlas/resolve: source weight estimated on this corpus");
    }

    // Records become the type's atoms, in the type's own kind.
    let mut atom_of: HashMap<&str, AtomId> = HashMap::new();
    for rec in resolver.records() {
        // A record is opened by a placed statement, so both are there; one
        // that is not is refused, never named or placed by a default.
        let (Some(e), Some(section)) = (rec.evidence.first(), section_of.get(&rec.id)) else {
            failures.push(failure(
                format!("record:{}", rec.id),
                PhaseFailureKind::Other,
                format!(
                    "`{}` record {} has no opening evidence or section; no atom",
                    t.name, rec.id
                ),
            ));
            continue;
        };
        let (document, surface, section) = (e.document.clone(), e.surface.clone(), section.clone());
        let mut first = ChunkRef::new(section.clone(), Some(surface.clone()));
        first.source_doc_id = Some(document.clone());
        if kind == TypeKind::Event {
            let event_type = EventType::from_str_repr(&t.name);
            let id = AtomId::event_content_hash(&rec.id, &event_type, &section, corpus_id);
            debug!(r#type = %t.name, record = %rec.id, atom = %id.as_str(), statements = rec.statements.len(), "atlas/resolve: record becomes an event atom");
            atom_of.insert(rec.id.as_str(), id.clone());
            atoms.events.push(Event {
                id,
                description: surface,
                event_type,
                participants: Vec::new(),
                evidence: vec![first],
                section_position: SectionPosition::section(section),
                causal_antecedents: Vec::new(),
                attributes: Map::new(),
                enrichment_depth: EnrichmentDepth::Extracted,
            });
            continue;
        }
        let ty = EntityType::from_str_repr(&t.name);
        let id = AtomId::exact_entity_content_hash(&rec.id, &ty, corpus_id);
        debug!(r#type = %t.name, record = %rec.id, atom = %id.as_str(), statements = rec.statements.len(), "atlas/resolve: record becomes an atom");
        atom_of.insert(rec.id.as_str(), id.clone());
        atoms.entities.push(Entity {
            id,
            canonical_name: surface,
            aliases: Vec::new(),
            entity_type: ty.clone(),
            first_appearance: first,
            description: String::new(),
            defining_quote: None,
            salience: 0.0,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: SignalProvenance::new(EXTRACTOR_ID, document, SignalKind::LlmBatch)
                .with_chunk(section),
            attributes: Map::new(),
            concept_kind: None,
        });
    }
    report.records = atom_of.len();

    // Each claim's subject is its statement's record.
    for (i, id) in statement_of {
        let claim = &mut atoms.claims[i];
        claim.subject = record_of
            .get(&id)
            .and_then(|r| atom_of.get(r.as_str()))
            .cloned();
        if claim.subject.is_none() {
            let label = refused.get(&id).copied().unwrap_or("undecided");
            failures.push(failure(
                format!("atom:{}", claim.id.as_str()),
                PhaseFailureKind::UnresolvedClaimSubject,
                format!(
                    "RESOLVE left statement {id} in no `{}` ({label}); the claim has no subject",
                    t.name
                ),
            ));
        }
    }
    report
}

fn unplaced(
    report: &mut RecordsReport,
    failures: &mut Vec<PhaseFailure>,
    claim: &Claim,
    kind: &str,
    type_name: &str,
    why: String,
) {
    report.unplaced += 1;
    debug!(claim = %claim.id.as_str(), kind, %why, "atlas/resolve: claim is no statement");
    failures.push(failure(
        format!("atom:{}", claim.id.as_str()),
        PhaseFailureKind::UnresolvedClaimSubject,
        format!("`{kind}` claim is no statement of a `{type_name}`: {why}; it has no subject"),
    ));
}

#[cfg(test)]
#[path = "resolution_records/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "resolution_records/declared_document_read_tests.rs"]
mod declared_document_read_tests;
