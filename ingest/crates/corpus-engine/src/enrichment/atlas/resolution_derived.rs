// SPDX-License-Identifier: AGPL-3.0-or-later
//! Derived attributes in the atlas build (`ontology::derived`;
//! ONTOLOGY_PRIMITIVES.md §8). For each attribute a type declares `derived`,
//! in the one order `DerivedPolicy::order` gives, every atom of the type gets
//! the value its path or fold reaches from it, or a counted absence; never a
//! guess. Steps walk the build as it stands: `subject` from a claim to its
//! particular and back, `document` to the one document `locate` finds, a
//! document field to the atoms a metadata source projected from it
//! (`Participants`), and a declared attribute to the atoms it names.
//!
//! RESOLVE makes the records of the types it decides, so an attribute of such
//! a type is derived after it and every other attribute before it
//! (`DeriveStage`); `recipe validate` refuses a before-RESOLVE derivation
//! that reads what RESOLVE makes.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;
use serde_json::Value;
use tracing::{debug, info};

use super::atoms::{Claim, Entity};
use super::resolution_documents::{locate, read_stamp, SectionDocuments, SourceDocument};
use super::resolution_records::{decides, BuildAtoms};
use super::resolution_sources::Participants;
use crate::enrichment::ontology::derived::{
    Condition, FoldBy, Named, PathExpr, SetDecl, DOCUMENT, SUBJECT,
};
use crate::enrichment::ontology::{DocumentStamp, OntologyPolicies, TypeIndex};
use crate::enrichment::reconciliation::identity_signals::fold_identity_value;

#[path = "resolution_derived/protocol.rs"]
mod protocol;

/// Which side of RESOLVE a derivation runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeriveStage {
    BeforeResolve,
    AfterResolve,
}

/// What a derivation gave one atom. Closed: an absence says which kind.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DerivedOutcome {
    Decided {
        values: Vec<String>,
        /// The `from` entry that decided, for a fold whose order matters.
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<usize>,
        /// The documents the decided values came through.
        documents: Vec<String>,
        /// Other values later inputs reached, which the decision outranked.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        superseded: Vec<String>,
    },
    /// No input reached a value.
    Nothing,
    /// Every input that reached anything reached several values.
    Ambiguous { values: Vec<String> },
    /// `agree`: the inputs reached different values.
    Conflict { values: Vec<String> },
    /// `most`, `earliest`, `latest`: several values share the top place.
    Tie { values: Vec<String> },
    /// `earliest`, `latest`: values were reached, none through a dated document.
    Unordered,
    /// A declared protocol could apply, but a requested qualification or a
    /// total report-time order is unavailable.
    Pending {
        values: Vec<String>,
        reasons: Vec<String>,
    },
}

impl DerivedOutcome {
    pub fn label(&self) -> &'static str {
        match self {
            DerivedOutcome::Decided { .. } => "decided",
            DerivedOutcome::Nothing => "nothing",
            DerivedOutcome::Ambiguous { .. } => "ambiguous",
            DerivedOutcome::Conflict { .. } => "conflict",
            DerivedOutcome::Tie { .. } => "tie",
            DerivedOutcome::Unordered => "unordered",
            DerivedOutcome::Pending { .. } => "pending",
        }
    }
}

/// Replayable provenance for one recipe-declared scalar protocol projection.
#[derive(Debug, Clone, Serialize)]
pub struct DerivedProtocolAudit {
    /// Content identity of the fold, its paths, qualifications and rule data.
    pub rule_fingerprint: String,
    /// Report time that selected the current state; never the transition's effective time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub as_of_report_time: Option<String>,
    /// Effective-time values on the selected reports, retained without ordering them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub effective_times: Vec<String>,
    /// Rules that matched or remained possible under missing qualifications.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rule_ids: Vec<String>,
    /// Every claim reached through the assigned-subject path, including excluded inputs.
    pub basis_claims: Vec<String>,
    /// The input claim → resolved record assignment used by this projection.
    pub assignment_dependencies: BTreeMap<String, String>,
    /// Supported states that remain possible when the fold is pending or conflicting.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,
    /// Every reached claim, its source document, citation, field evidence and disposition.
    pub history: Vec<Value>,
    /// Why no unique state was projected, if applicable.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

/// One atom's derivation, as the build keeps it beside the atlas.
#[derive(Debug, Clone, Serialize)]
pub struct DerivedValue {
    pub stage: DeriveStage,
    #[serde(rename = "type")]
    pub type_name: String,
    pub attribute: String,
    pub atom: String,
    pub derived: String,
    #[serde(flatten)]
    pub outcome: DerivedOutcome,
    /// Atoms a `[!set]` filter dropped on the way.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<String>,
    /// The value the atom held before, when the derivation changed it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaced: Option<Value>,
    /// Rule, source, and assignment lineage for a protocol fold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<DerivedProtocolAudit>,
    /// The derivation as the value's source (C3). Code computes it from the
    /// declared path and fold, over inputs whose own precisions they carry;
    /// no precision of the fold's output is declared or estimated yet.
    pub by: crate::enrichment::atlas::precision::SourcePrecision,
}

/// Per attribute, what one stage did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DerivedTally {
    pub atoms: usize,
    pub outcomes: BTreeMap<&'static str, usize>,
    /// Atoms a `[!set]` filter dropped, over every derivation of the attribute.
    pub excluded: usize,
    /// Atoms of a set's type lacking an attribute it reads: in no set, out of no set.
    pub unjudged: usize,
}

/// `(type.attribute, tally)` for each attribute one stage derived.
pub type DerivedReport = BTreeMap<String, DerivedTally>;

/// Derive every attribute of `stage`, in the declared order; `sink` sees each
/// atom's derivation. Refuses a declaration whose order cannot be computed.
pub fn derive_attributes(
    atoms: &mut BuildAtoms<'_>,
    documents: &SectionDocuments,
    participants: &Participants,
    policies: &OntologyPolicies,
    stage: DeriveStage,
    sink: &mut (dyn FnMut(&DerivedValue) + Send),
) -> Result<DerivedReport, String> {
    let declared = &policies.derivation.derived;
    let index = TypeIndex::from_policies(policies);
    let order = declared.order(&policies.shape.types)?;
    let mut report = DerivedReport::new();
    for (type_name, attr, id) in order {
        let after = decides(&index, type_name);
        if (stage == DeriveStage::AfterResolve) != after {
            continue;
        }
        let is_it = |t: &str| t == type_name || index.is_a(t, type_name);
        let (mut results, mut tally) = (Vec::new(), DerivedTally::default());
        {
            let graph = Graph::new(atoms, documents, participants, policies, &index)?;
            let targets = atoms
                .entities
                .iter()
                .enumerate()
                .filter(|(_, e)| is_it(e.entity_type.as_str_repr()))
                .map(|(i, _)| Node::Entity(i))
                .chain(
                    atoms
                        .claims
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| c.claim_kind.as_deref().is_some_and(is_it))
                        .map(|(j, _)| Node::Claim(j)),
                );
            for node in targets {
                let mut fx = Effects::default();
                let doc = graph.doc_of(&node);
                let (outcome, protocol) = graph.derive(id, &node, doc, &mut fx)?;
                tally.excluded += fx.excluded.len();
                tally.unjudged += fx.unjudged;
                results.push((node, outcome, fx.excluded, protocol));
            }
        }
        for (node, outcome, excluded, protocol) in results {
            let (atom_id, attributes) = match node {
                Node::Entity(i) => (
                    atoms.entities[i].id.as_str().to_string(),
                    &mut atoms.entities[i].attributes,
                ),
                Node::Claim(j) => (
                    atoms.claims[j].id.as_str().to_string(),
                    &mut atoms.claims[j].attributes,
                ),
                Node::Document(_) => continue,
            };
            let previous = attributes.remove(attr);
            if let DerivedOutcome::Decided { values, .. } = &outcome {
                let value = match values.as_slice() {
                    [one] => Value::String(one.clone()),
                    many => Value::Array(many.iter().cloned().map(Value::String).collect()),
                };
                attributes.insert(attr.to_string(), value);
            }
            let replaced = previous.filter(|p| attributes.get(attr) != Some(p));
            tally.atoms += 1;
            *tally.outcomes.entry(outcome.label()).or_default() += 1;
            debug!(r#type = type_name, attribute = attr, atom = %atom_id, outcome = outcome.label(), protocol = protocol.is_some(), "atlas/derive: attribute derived");
            sink(&DerivedValue {
                stage,
                type_name: type_name.to_string(),
                attribute: attr.to_string(),
                atom: atom_id,
                derived: id.to_string(),
                outcome,
                excluded: excluded.into_iter().collect(),
                replaced,
                protocol,
                by: crate::enrichment::atlas::precision::SourcePrecision::new(
                    format!("derived:{id}"),
                    crate::enrichment::atlas::precision::Precision::Unmeasured,
                ),
            });
        }
        info!(r#type = type_name, attribute = attr, derived = id, ?stage, atoms = tally.atoms, outcomes = ?tally.outcomes, excluded = tally.excluded, unjudged = tally.unjudged, "atlas/derive: attribute done");
        report.insert(format!("{type_name}.{attr}"), tally);
    }
    Ok(report)
}

/// A place a path walk stands.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Node {
    Entity(usize),
    Claim(usize),
    Document(String),
}

/// Where a walk stands and the last document it came through.
type Reached = BTreeSet<(Node, Option<String>)>;

/// What one derivation dropped on its way.
#[derive(Default)]
struct Effects {
    excluded: BTreeSet<String>,
    unjudged: usize,
}

/// The build, indexed for walking. Rebuilt per attribute, because an earlier
/// attribute's values are steps for a later one.
struct Graph<'a> {
    entities: &'a [Entity],
    claims: &'a [Claim],
    entity_at: HashMap<&'a str, usize>,
    about: HashMap<&'a str, Vec<usize>>,
    claim_doc: Vec<Option<&'a str>>,
    docs: HashMap<&'a str, &'a SourceDocument>,
    participants: &'a Participants,
    clock: HashMap<&'a str, String>,
    /// Attribute → target atom → the atoms naming it there, for `^attribute`.
    named_by: HashMap<String, HashMap<String, Vec<Node>>>,
    policies: &'a OntologyPolicies,
    index: &'a TypeIndex<'a>,
    parsed: HashMap<&'a str, Vec<PathExpr>>,
}

impl<'a> Graph<'a> {
    fn new(
        atoms: &'a BuildAtoms<'_>,
        documents: &'a SectionDocuments,
        participants: &'a Participants,
        policies: &'a OntologyPolicies,
        index: &'a TypeIndex<'a>,
    ) -> Result<Self, String> {
        let declared = &policies.derivation.derived;
        let mut parsed = HashMap::new();
        for id in declared
            .paths
            .iter()
            .map(|p| p.id.as_str())
            .chain(declared.folds.iter().map(|f| f.id.as_str()))
        {
            parsed.insert(id, declared.exprs(id)?);
        }
        let inverted: BTreeSet<String> = parsed
            .values()
            .flatten()
            .flat_map(PathExpr::steps)
            .filter(|(n, inv)| *inv && *n != SUBJECT)
            .map(|(n, _)| n.to_string())
            .collect();
        let (entities, claims) = (atoms.entities.as_slice(), atoms.claims.as_slice());
        let mut named_by: HashMap<String, HashMap<String, Vec<Node>>> = HashMap::new();
        let nodes = entities
            .iter()
            .enumerate()
            .map(|(i, e)| (Node::Entity(i), &e.attributes))
            .chain(
                claims
                    .iter()
                    .enumerate()
                    .map(|(j, c)| (Node::Claim(j), &c.attributes)),
            );
        for (node, attributes) in nodes {
            for attr in &inverted {
                for target in scalars(attributes.get(attr)) {
                    named_by
                        .entry(attr.clone())
                        .or_default()
                        .entry(target)
                        .or_default()
                        .push(node.clone());
                }
            }
        }
        let mut about: HashMap<&str, Vec<usize>> = HashMap::new();
        for (j, c) in claims.iter().enumerate() {
            if let Some(s) = &c.subject {
                about.entry(s.as_str()).or_default().push(j);
            }
        }
        let docs = documents.by_key();
        let date_field = policies
            .change
            .document
            .as_ref()
            .and_then(|d| d.date.as_deref());
        let clock = date_field
            .map(|f| {
                docs.iter()
                    .filter_map(|(k, d)| match read_stamp(&d.fields, DocumentStamp::Date, f) {
                        Ok(t) => Some((*k, t)),
                        Err(why) => {
                            debug!(document = %k, %why, "atlas/derive: date unreadable; the document is off the clock");
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            entities,
            claims,
            entity_at: entities
                .iter()
                .enumerate()
                .map(|(i, e)| (e.id.as_str(), i))
                .collect(),
            about,
            claim_doc: claims
                .iter()
                .map(|c| match locate(c, documents) {
                    Ok(d) => Some(d.key.as_str()),
                    Err(why) => {
                        debug!(claim = %c.id.as_str(), %why, "atlas/derive: claim in no one document; its `document` step reaches nothing");
                        None
                    }
                })
                .collect(),
            docs,
            participants,
            clock,
            named_by,
            policies,
            index,
            parsed,
        })
    }

    fn doc_of(&self, node: &Node) -> Option<String> {
        match node {
            Node::Claim(j) => self.claim_doc[*j].map(str::to_string),
            Node::Document(k) => Some(k.clone()),
            Node::Entity(_) => None,
        }
    }

    /// The value `id` (a path or a fold) gives `node`.
    fn derive(
        &self,
        id: &str,
        node: &Node,
        doc: Option<String>,
        fx: &mut Effects,
    ) -> Result<(DerivedOutcome, Option<DerivedProtocolAudit>), String> {
        if let Some(Named::FoldDecl(fold)) = self.policies.derivation.derived.get(id) {
            if fold.by == FoldBy::Protocol {
                let (outcome, audit) = protocol::derive(self, fold, node, doc, fx)?;
                return Ok((outcome, Some(audit)));
            }
        }
        let (by, exprs) = match self.policies.derivation.derived.get(id) {
            Some(Named::FoldDecl(f)) => (f.by, &self.parsed[id]),
            Some(Named::PathDecl(_)) => (FoldBy::All, &self.parsed[id]),
            _ => return Err(format!("`{id}` is no declared path or fold")),
        };
        let start: Reached = [(node.clone(), doc)].into();
        let inputs: Vec<BTreeMap<String, BTreeSet<Option<String>>>> = exprs
            .iter()
            .map(|e| {
                let mut values: BTreeMap<String, BTreeSet<Option<String>>> = BTreeMap::new();
                for (n, d) in self.walk(e, start.clone(), fx)? {
                    if let Node::Entity(i) = n {
                        values
                            .entry(self.entities[i].id.as_str().to_string())
                            .or_default()
                            .insert(d);
                    }
                }
                Ok(values)
            })
            .collect::<Result<_, String>>()?;
        Ok((decide(by, &inputs, &self.clock), None))
    }

    fn walk(&self, p: &PathExpr, from: Reached, fx: &mut Effects) -> Result<Reached, String> {
        Ok(match p {
            PathExpr::Seq(ps) => {
                let mut at = from;
                for p in ps {
                    at = self.walk(p, at, fx)?;
                }
                at
            }
            PathExpr::Alt(ps) => {
                let mut out = Reached::new();
                for p in ps {
                    out.extend(self.walk(p, from.clone(), fx)?);
                }
                out
            }
            PathExpr::Filter { inner, set, keep } => {
                let Some(Named::SetDecl(s)) = self.policies.derivation.derived.get(set) else {
                    return Err(format!("`[{set}]` names no declared set"));
                };
                let mut out = Reached::new();
                for (n, d) in self.walk(inner, from, fx)? {
                    match (self.member(s, &n), keep) {
                        (None, _) => fx.unjudged += 1,
                        (Some(m), true) if m => {
                            out.insert((n, d));
                        }
                        (Some(m), false) if !m => {
                            out.insert((n, d));
                        }
                        (Some(_), false) => {
                            if let Node::Entity(i) = n {
                                fx.excluded.insert(self.entities[i].id.as_str().to_string());
                            }
                        }
                        (Some(_), true) => {}
                    }
                }
                out
            }
            PathExpr::Step { name, inverse } => {
                let mut out = Reached::new();
                for (n, d) in from {
                    self.step(name, *inverse, &n, d, fx, &mut out)?;
                }
                out
            }
        })
    }

    fn step(
        &self,
        name: &str,
        inverse: bool,
        node: &Node,
        doc: Option<String>,
        fx: &mut Effects,
        out: &mut Reached,
    ) -> Result<(), String> {
        let entity = |id: &str| self.entity_at.get(id).map(|&i| Node::Entity(i));
        match (name, inverse, node) {
            (SUBJECT, true, Node::Entity(i)) => {
                for &j in self
                    .about
                    .get(self.entities[*i].id.as_str())
                    .into_iter()
                    .flatten()
                {
                    let d = self.claim_doc[j].map(str::to_string).or(doc.clone());
                    out.insert((Node::Claim(j), d));
                }
            }
            (SUBJECT, false, Node::Claim(j)) => {
                if let Some(n) = self.claims[*j]
                    .subject
                    .as_ref()
                    .and_then(|s| entity(s.as_str()))
                {
                    out.insert((n, doc));
                }
            }
            (DOCUMENT, false, Node::Claim(j)) => {
                if let Some(k) = self.claim_doc[*j] {
                    out.insert((Node::Document(k.to_string()), Some(k.to_string())));
                }
            }
            (field, false, Node::Document(k)) => {
                for (f, id) in self.participants.get(k).into_iter().flatten() {
                    if f.as_str() == field {
                        if let Some(n) = entity(id.as_str()) {
                            out.insert((n, doc.clone()));
                        }
                    }
                }
            }
            (id, false, Node::Entity(_) | Node::Claim(_)) if self.parsed.contains_key(id) => {
                match self.derive(id, node, doc.clone(), fx)?.0 {
                    DerivedOutcome::Decided { values, .. } => {
                        out.extend(
                            values
                                .iter()
                                .filter_map(|v| entity(v))
                                .map(|n| (n, doc.clone())),
                        );
                    }
                    other => debug!(
                        id,
                        outcome = other.label(),
                        "atlas/derive: an inner derivation decided nothing"
                    ),
                }
            }
            (attr, false, Node::Entity(_) | Node::Claim(_)) => {
                let attributes = match node {
                    Node::Entity(i) => &self.entities[*i].attributes,
                    Node::Claim(j) => &self.claims[*j].attributes,
                    Node::Document(_) => return Ok(()),
                };
                for id in scalars(attributes.get(attr)) {
                    if let Some(n) = entity(&id) {
                        out.insert((n, doc.clone()));
                    }
                }
            }
            (attr, true, Node::Entity(i)) => {
                let id = self.entities[*i].id.as_str();
                for n in self
                    .named_by
                    .get(attr)
                    .and_then(|m| m.get(id))
                    .into_iter()
                    .flatten()
                {
                    let d = match n {
                        Node::Claim(j) => self.claim_doc[*j].map(str::to_string).or(doc.clone()),
                        _ => doc.clone(),
                    };
                    out.insert((n.clone(), d));
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Whether `node` is in `set`: `None` when it is of the set's kind but
    /// lacks an attribute a condition reads, which is no answer either way.
    fn member(&self, set: &SetDecl, node: &Node) -> Option<bool> {
        let is_it = |t: &str| t == set.of || self.index.is_a(t, &set.of);
        let fields = match node {
            Node::Document(k) if set.of == DOCUMENT => &self.docs.get(k.as_str())?.fields,
            Node::Entity(i) if is_it(self.entities[*i].entity_type.as_str_repr()) => {
                &self.entities[*i].attributes
            }
            Node::Claim(j) if self.claims[*j].claim_kind.as_deref().is_some_and(is_it) => {
                &self.claims[*j].attributes
            }
            _ => return Some(false),
        };
        for (attr, cond) in &set.conditions {
            let values: Vec<String> = scalars(fields.get(attr))
                .iter()
                .filter_map(|v| fold_identity_value(v))
                .collect();
            if values.is_empty() {
                return None;
            }
            let wanted = |w: &str| fold_identity_value(w);
            let holds = values.iter().any(|v| match cond {
                Condition::Is(w) => wanted(w).as_deref() == Some(v.as_str()),
                Condition::In(ws) => ws.iter().any(|w| wanted(w).as_deref() == Some(v.as_str())),
                Condition::Suffix { suffix } => {
                    wanted(suffix).is_some_and(|s| *v == s || v.ends_with(&format!(" {s}")))
                }
            });
            if !holds {
                return Some(false);
            }
        }
        Some(true)
    }
}

/// The fold registry's arms. `inputs[k]` maps each value input `k` reached to
/// the documents it came through.
fn decide(
    by: FoldBy,
    inputs: &[BTreeMap<String, BTreeSet<Option<String>>>],
    clock: &HashMap<&str, String>,
) -> DerivedOutcome {
    let mut all: BTreeMap<&str, BTreeSet<&Option<String>>> = BTreeMap::new();
    for m in inputs {
        for (v, ds) in m {
            all.entry(v.as_str()).or_default().extend(ds);
        }
    }
    let docs = |v: &str| -> Vec<String> {
        all.get(v)
            .into_iter()
            .flatten()
            .filter_map(|d| d.as_ref().cloned())
            .collect()
    };
    let decided = |values: Vec<&str>, input: Option<usize>, superseded: Vec<String>| {
        let mut documents: Vec<String> = values.iter().flat_map(|v| docs(v)).collect();
        documents.sort();
        documents.dedup();
        DerivedOutcome::Decided {
            values: values.into_iter().map(str::to_string).collect(),
            input,
            documents,
            superseded,
        }
    };
    let names = |vs: &[&str]| vs.iter().map(|v| v.to_string()).collect::<Vec<_>>();
    let values: Vec<&str> = all.keys().copied().collect();
    match by {
        FoldBy::First => {
            let mut ambiguous = BTreeSet::new();
            for (k, m) in inputs.iter().enumerate() {
                match m.len() {
                    0 => continue,
                    1 => {
                        let Some(v) = m.keys().next().map(String::as_str) else {
                            continue;
                        };
                        let superseded: BTreeSet<String> = inputs[k + 1..]
                            .iter()
                            .flat_map(|m| m.keys())
                            .filter(|w| w.as_str() != v)
                            .cloned()
                            .collect();
                        return decided(vec![v], Some(k), superseded.into_iter().collect());
                    }
                    _ => ambiguous.extend(m.keys().cloned()),
                }
            }
            if ambiguous.is_empty() {
                DerivedOutcome::Nothing
            } else {
                DerivedOutcome::Ambiguous {
                    values: ambiguous.into_iter().collect(),
                }
            }
        }
        FoldBy::Agree => match values.as_slice() {
            [] => DerivedOutcome::Nothing,
            [v] => decided(vec![v], None, Vec::new()),
            vs => DerivedOutcome::Conflict { values: names(vs) },
        },
        FoldBy::All => match values.as_slice() {
            [] => DerivedOutcome::Nothing,
            vs => decided(vs.to_vec(), None, Vec::new()),
        },
        FoldBy::Most => {
            let count = |v: &str| all.get(v).map_or(0, BTreeSet::len);
            let top = values.iter().map(|v| count(v)).max().unwrap_or(0);
            match values
                .iter()
                .filter(|v| count(v) == top)
                .copied()
                .collect::<Vec<_>>()
                .as_slice()
            {
                [] => DerivedOutcome::Nothing,
                [v] => decided(vec![v], None, Vec::new()),
                vs => DerivedOutcome::Tie { values: names(vs) },
            }
        }
        FoldBy::Earliest | FoldBy::Latest => {
            let when = |v: &str| -> Option<&String> {
                let dated = all.get(v)?.iter().filter_map(|d| clock.get(d.as_deref()?));
                if by == FoldBy::Earliest {
                    dated.min()
                } else {
                    dated.max()
                }
            };
            let dated: Vec<(&str, &String)> =
                values.iter().filter_map(|v| Some((*v, when(v)?))).collect();
            let edge = if by == FoldBy::Earliest {
                dated.iter().map(|(_, t)| *t).min()
            } else {
                dated.iter().map(|(_, t)| *t).max()
            };
            match (values.is_empty(), edge) {
                (true, _) => DerivedOutcome::Nothing,
                (false, None) => DerivedOutcome::Unordered,
                (false, Some(t)) => {
                    match dated
                        .iter()
                        .filter(|(_, w)| *w == t)
                        .map(|(v, _)| *v)
                        .collect::<Vec<_>>()
                        .as_slice()
                    {
                        [v] => decided(vec![v], None, Vec::new()),
                        vs => DerivedOutcome::Tie { values: names(vs) },
                    }
                }
            }
        }
        FoldBy::Protocol => unreachable!("protocol folds are evaluated over assigned claims"),
    }
}

/// An attribute value's scalars: a string or number, or each of a list. A
/// `ref`'s atom ids read the same way.
fn scalars(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Number(n)) => vec![n.to_string()],
        Some(Value::Array(xs)) => xs.iter().flat_map(|x| scalars(Some(x))).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
#[path = "resolution_derived/tests.rs"]
mod tests;
