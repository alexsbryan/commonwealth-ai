// SPDX-License-Identifier: AGPL-3.0-or-later
//! Typed entity atoms from a document's own metadata fields, no model call
//! (`source = { metadata = [...], attributes = {...} }`,
//! `ONTOLOGY_PRIMITIVES.md` §8, Shape row).
//!
//! Two halves. [`project_source_atoms`] walks every document the build loaded
//! ([`SectionDocuments`], the rows `change.document` stamps from) and makes
//! one atom per distinct identity value of each sourced type, reading a field
//! only through the readers the recipe declared ([`FieldReader`]); no field
//! name is in code. [`fold_source_atoms`] runs inside Phase 3a and adds no
//! merge rule: the reconciler's strict `ExternalIdSignal` names the model
//! atoms that carry a projected atom's identity value, the resolver's veto
//! (`merge_permitted`) may still refuse one, and the resolver's own
//! `merge_into_existing` folds each mention in.

use std::collections::{BTreeMap, HashMap, HashSet};

use mailparse::{MailAddr, SingleInfo};
use serde_json::{Map, Value};
use tracing::{debug, info, trace, warn};

use super::atoms::{AtomId, ChunkRef, Entity, SignalKind, SignalProvenance};
use super::resolution::{fold, merge_into_existing};
use super::resolution_documents::{failure, SectionDocuments};
use super::resolution_identity::{merge_permitted, MergeEvidence};
use super::resolution_ontology::ResolutionPolicy;
use crate::enrichment::ontology::{
    FieldReader, MetadataSourceDecl, OntologyPolicies, SourceDecl, TypeIndex, TypeKind,
    DOCUMENT_COUNT_ATTR,
};
use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntitySketch, EntityType};
use crate::enrichment::pipeline::types::{PhaseFailure, PhaseFailureKind};
use crate::enrichment::reconciliation::identity_signals::{fold_identity_value, ExternalIdSignal};
use crate::enrichment::reconciliation::multi_origin::reconcile_with_signals;
use crate::enrichment::reconciliation::{MergeSignalCheck, ReconciliationPolicy};

/// `SignalProvenance::extractor_id` of a projected atom.
pub const SOURCE_EXTRACTOR_ID: &str = "document_fields";

/// The projected atoms and what producing them saw.
#[derive(Debug, Clone, Default)]
pub struct SourceProjection {
    /// One atom per distinct identity value per sourced type, id from that
    /// value (`AtomId::exact_entity_content_hash`).
    pub atoms: Vec<Entity>,
    pub report: SourceReport,
}

/// What projection saw, per sourced type, for the resolve step's output.
#[derive(Debug, Clone, Default)]
pub struct SourceReport {
    /// Documents walked.
    pub documents: usize,
    pub types: BTreeMap<String, SourceTypeReport>,
    /// One record per field a document holds that its readers cannot read.
    pub failures: Vec<PhaseFailure>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceTypeReport {
    /// The fields read and the identity keys, as declared.
    pub fields: Vec<String>,
    pub identity: Vec<String>,
    /// Atoms projected: distinct identity values.
    pub projected: usize,
    /// Sightings skipped because `exclude` names their identity value.
    pub excluded: usize,
    /// Sightings with no identity value (a bare address read by `display_name`).
    pub without_identity: usize,
    /// Field → documents that do not carry it.
    pub absent: BTreeMap<String, usize>,
    /// Fields present but unreadable, each also a failure record.
    pub unreadable: usize,
    /// `refs` attribute → atoms it links to a projected atom of its target type.
    pub refs_linked: BTreeMap<String, usize>,
    /// `refs` attribute → atoms carrying a value the target never projected
    /// (excluded, or absent from every document).
    pub refs_unlinked: BTreeMap<String, usize>,
}

/// What the fold did, returned on `ResolutionOutput::sources`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceFoldReport {
    /// Projected atoms the atlas holds.
    pub projected: usize,
    /// Type → model atoms folded into a projected atom on the identity key.
    pub merged: BTreeMap<String, usize>,
    /// Type → model atoms whose key agreed and the declared ontology refused.
    pub refused: BTreeMap<String, usize>,
    /// Type → projected atoms no document named, renamed after the most
    /// salient model atom folded in (the key stays an alias).
    pub named: BTreeMap<String, usize>,
}

impl SourceReport {
    /// One line per sourced type for the resolve step's output.
    pub fn summary_lines(&self, fold: &SourceFoldReport) -> Vec<String> {
        self.types
            .iter()
            .map(|(t, r)| {
                let absent = r
                    .absent
                    .iter()
                    .map(|(f, n)| format!("{f} ×{n}"))
                    .collect::<Vec<_>>();
                format!(
                    "source {t} ← {}: {} atom(s) from {} document(s); {} model atom(s) merged \
                     on {} ({} refused, {} named an atom); {} excluded, {} without identity, \
                     {} unreadable field(s); absent: {}",
                    r.fields.join(", "),
                    r.projected,
                    self.documents,
                    fold.merged.get(t).copied().unwrap_or(0),
                    r.identity.join(" + "),
                    fold.refused.get(t).copied().unwrap_or(0),
                    fold.named.get(t).copied().unwrap_or(0),
                    r.excluded,
                    r.without_identity,
                    r.unreadable,
                    if absent.is_empty() {
                        "none".to_string()
                    } else {
                        absent.join(", ")
                    }
                )
            })
            .collect()
    }
}

/// Project every type that declares a metadata source. Refuses a declaration
/// that cannot name an identity value (`MetadataSourceDecl::identity_readers`)
/// rather than projecting under a guessed key.
pub fn project_source_atoms(
    documents: &SectionDocuments,
    policies: &OntologyPolicies,
    corpus_id: &str,
) -> Result<SourceProjection, String> {
    let index = TypeIndex::from_policies(policies);
    let docs = documents.each_document();
    let mut out = SourceProjection::default();
    out.report.documents = docs.len();
    // (type, identity key) -> atom id, for `refs`; and the refs to link once every type is projected
    let mut keyed: HashMap<(String, String), AtomId> = HashMap::new();
    let mut pending: Vec<(usize, String, String, String, Vec<(String, usize)>)> = Vec::new();
    for t in &policies.shape.types {
        let Some(SourceDecl::Metadata(src)) = &t.source else {
            continue;
        };
        if t.kind != TypeKind::Entity {
            return Err(format!(
                "ontology type `{}` reads document fields but is not an entity type",
                t.name
            ));
        }
        let identity = src
            .identity_readers(index.effective_identity(&t.name))
            .map_err(|e| format!("ontology type `{}` (metadata source): {e}", t.name))?;
        let mut report = SourceTypeReport {
            fields: src.metadata.clone(),
            identity: identity.iter().map(|(k, _)| k.to_string()).collect(),
            ..Default::default()
        };
        let accs = project_type(&t.name, src, &identity, &docs, &mut report, &mut out.report);
        report.projected = accs.len();
        let ty = EntityType::from_str_repr(&t.name);
        for mut a in accs {
            let refs = std::mem::take(&mut a.refs);
            let key = a.key.clone();
            let e = a.into_entity(src, &identity, &ty, corpus_id);
            for (attr, vals) in refs {
                let of = src.refs[&attr].of.clone();
                pending.push((out.atoms.len(), t.name.clone(), attr, of, vals));
            }
            keyed.insert((t.name.clone(), key), e.id.clone());
            out.atoms.push(e);
        }
        info!(
            ty = %t.name,
            projected = report.projected,
            excluded = report.excluded,
            unreadable = report.unreadable,
            "atlas/resolution sources: projected from document fields"
        );
        out.report.types.insert(t.name.clone(), report);
    }
    // refs: the most frequent value whose target the build projected; a value it
    // excluded or never saw links nothing, and is counted
    for (i, ty, attr, of, vals) in pending {
        let mut ranked = vals;
        ranked.sort_by(|a, b| b.1.cmp(&a.1));
        let hit = ranked.iter().find_map(|(v, _)| keyed.get(&(of.clone(), v.clone())));
        let report = out.report.types.entry(ty).or_default();
        match hit {
            Some(id) => {
                out.atoms[i].attributes.insert(attr.clone(), Value::String(id.as_str().to_string()));
                *report.refs_linked.entry(attr).or_default() += 1;
            }
            None => {
                trace!(atom = %out.atoms[i].canonical_name, attr, values = ?ranked, "atlas/resolution sources: ref links nothing");
                *report.refs_unlinked.entry(attr).or_default() += 1;
            }
        }
    }
    for (ty, r) in &out.report.types {
        if !r.refs_linked.is_empty() || !r.refs_unlinked.is_empty() {
            info!(ty = %ty, linked = ?r.refs_linked, unlinked = ?r.refs_unlinked, "atlas/resolution sources: refs linked to projected atoms");
        }
    }
    // Salience is how many documents carry the value, against the most-seen.
    let count = |e: &Entity| {
        e.attributes
            .get(DOCUMENT_COUNT_ATTR)
            .and_then(Value::as_u64)
    };
    let max = out.atoms.iter().filter_map(count).max().unwrap_or(1).max(1);
    for e in &mut out.atoms {
        e.salience = count(e).unwrap_or(0) as f32 / max as f32;
    }
    Ok(out)
}

/// One mailbox of an address list, or the whole value under `value`.
struct Item {
    addr: Option<String>,
    name: Option<String>,
}

/// Everything seen for one identity value.
struct Accum {
    key: String,
    section: String,
    document: String,
    documents: HashSet<String>,
    /// Attribute → each value read, with its count, first-seen first.
    values: BTreeMap<String, Vec<(String, usize)>>,
    /// `refs` attribute → each folded target identity value read, with its count.
    refs: BTreeMap<String, Vec<(String, usize)>>,
}

fn project_type(
    type_name: &str,
    src: &MetadataSourceDecl,
    identity: &[(&str, FieldReader)],
    docs: &[(&str, &str, &Map<String, Value>)],
    report: &mut SourceTypeReport,
    all: &mut SourceReport,
) -> Vec<Accum> {
    let reads_addresses = src.reads_addresses();
    let excluded: HashSet<String> = src
        .exclude
        .iter()
        .filter_map(|e| fold_identity_value(e))
        .collect();
    let mut accs: Vec<Accum> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for (section, doc, fields) in docs {
        for field in &src.metadata {
            let unreadable = |why: String, all: &mut SourceReport| {
                debug!(document = doc, field, %why, "atlas/resolution sources: field unreadable");
                all.failures.push(failure(
                    format!("document:{doc}"),
                    PhaseFailureKind::UnreadableDocumentField,
                    format!("`{field}` {why}; no `{type_name}` projected from it"),
                ));
            };
            let scalars = match fields.get(field.as_str()).map(scalars_of) {
                None | Some(Ok(None)) => {
                    trace!(
                        document = doc,
                        field,
                        "atlas/resolution sources: field absent"
                    );
                    *report.absent.entry(field.clone()).or_default() += 1;
                    continue;
                }
                Some(Err(why)) => {
                    report.unreadable += 1;
                    unreadable(why, all);
                    continue;
                }
                Some(Ok(Some(s))) => s,
            };
            for scalar in scalars {
                let items = if !reads_addresses {
                    vec![Item {
                        addr: None,
                        name: None,
                    }]
                } else {
                    match mailboxes(&scalar) {
                        Ok(m) => m,
                        Err(why) => {
                            report.unreadable += 1;
                            unreadable(format!("= {scalar:?} {why}"), all);
                            continue;
                        }
                    }
                };
                for item in items {
                    let read = |r: FieldReader| read_item(r, &item, &scalar);
                    let folded: Option<Vec<String>> = identity
                        .iter()
                        .map(|(_, r)| read(*r).as_deref().and_then(fold_identity_value))
                        .collect();
                    let Some(folded) = folded else {
                        trace!(
                            document = doc,
                            field,
                            "atlas/resolution sources: no identity value"
                        );
                        report.without_identity += 1;
                        continue;
                    };
                    if folded.iter().any(|f| excluded.contains(f)) {
                        trace!(document = doc, field, key = ?folded, "atlas/resolution sources: excluded");
                        report.excluded += 1;
                        continue;
                    }
                    let key = folded.join("\u{1f}");
                    let i = *by_key.entry(key.clone()).or_insert_with(|| {
                        accs.push(Accum {
                            key,
                            section: section.to_string(),
                            document: doc.to_string(),
                            documents: HashSet::new(),
                            values: BTreeMap::new(),
                            refs: BTreeMap::new(),
                        });
                        accs.len() - 1
                    });
                    let acc = &mut accs[i];
                    acc.documents.insert(doc.to_string());
                    for (attr, r) in &src.refs {
                        if let Some(v) = read(r.reader).as_deref().and_then(fold_identity_value) {
                            let seen = acc.refs.entry(attr.clone()).or_default();
                            match seen.iter_mut().find(|(s, _)| *s == v) {
                                Some((_, n)) => *n += 1,
                                None => seen.push((v, 1)),
                            }
                        }
                    }
                    for (attr, r) in &src.attributes {
                        if let Some(v) = read(*r) {
                            let seen = acc.values.entry(attr.clone()).or_default();
                            match seen.iter_mut().find(|(s, _)| *s == v) {
                                Some((_, n)) => *n += 1,
                                None => seen.push((v, 1)),
                            }
                        }
                    }
                }
            }
        }
    }
    accs
}

impl Accum {
    fn into_entity(
        self,
        src: &MetadataSourceDecl,
        identity: &[(&str, FieldReader)],
        ty: &EntityType,
        corpus_id: &str,
    ) -> Entity {
        // Each attribute takes its most frequent value, ties to the first seen.
        let chosen = |vals: &[(String, usize)]| {
            vals.iter()
                .fold(None::<&(String, usize)>, |best, v| match best {
                    Some(b) if b.1 >= v.1 => Some(b),
                    _ => Some(v),
                })
                .map(|(s, _)| s.clone())
        };
        let mut attributes = Map::new();
        for (attr, vals) in &self.values {
            if let Some(v) = chosen(vals) {
                attributes.insert(attr.clone(), Value::String(v));
            }
        }
        attributes.insert(
            DOCUMENT_COUNT_ATTR.to_string(),
            Value::from(self.documents.len()),
        );
        // The display name when the source reads one, else the identity value
        // (which a model atom's name replaces in the fold).
        let names = src
            .attributes
            .iter()
            .find(|(_, r)| **r == FieldReader::DisplayName)
            .and_then(|(a, _)| self.values.get(a));
        let canonical_name = names
            .and_then(|v| chosen(v))
            .unwrap_or_else(|| key_name(&attributes, identity.iter().map(|(k, _)| *k)));
        let mut aliases: Vec<String> = Vec::new();
        for (n, _) in names.into_iter().flatten() {
            if !n.eq_ignore_ascii_case(&canonical_name)
                && !aliases.iter().any(|a| a.eq_ignore_ascii_case(n))
            {
                aliases.push(n.clone());
            }
        }
        Entity {
            id: AtomId::exact_entity_content_hash(&self.key, ty, corpus_id),
            canonical_name,
            aliases,
            entity_type: ty.clone(),
            first_appearance: ChunkRef::new(self.section, None),
            description: String::new(),
            defining_quote: None,
            salience: 0.0,
            enrichment_depth: EnrichmentDepth::Structural,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: SignalProvenance::new(
                SOURCE_EXTRACTOR_ID,
                self.document,
                SignalKind::DocumentField,
            ),
            attributes,
            concept_kind: None,
        }
    }
}

/// The name of a projected atom no document named: its identity values in key
/// order. One decider for naming it (`into_entity`) and for recognising it in
/// the fold, so the two cannot drift.
fn key_name<'a>(
    attributes: &Map<String, Value>,
    keys: impl IntoIterator<Item = &'a str>,
) -> String {
    keys.into_iter()
        .filter_map(|k| attributes.get(k).and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A field's value as the scalars a reader reads: a string or number, or each
/// one of a list. `Ok(None)` when it holds nothing (null, blank, empty list).
fn scalars_of(v: &Value) -> Result<Option<Vec<String>>, String> {
    let one = |x: &Value| match x {
        Value::String(s) => Ok(s.trim().to_string()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Null => Ok(String::new()),
        Value::Bool(_) => Err("holds a boolean".to_string()),
        Value::Array(_) => Err("holds a nested list".to_string()),
        Value::Object(_) => Err("holds an object".to_string()),
    };
    let all = match v {
        Value::Array(xs) => xs.iter().map(one).collect::<Result<Vec<_>, _>>()?,
        x => vec![one(x)?],
    };
    let all: Vec<String> = all.into_iter().filter(|s| !s.is_empty()).collect();
    Ok((!all.is_empty()).then_some(all))
}

/// The mailboxes of an RFC 5322 address list (groups flattened).
fn mailboxes(list: &str) -> Result<Vec<Item>, String> {
    let parsed = mailparse::addrparse(list).map_err(|e| format!("is not an address list ({e})"))?;
    let item = |s: &SingleInfo| Item {
        addr: Some(s.addr.trim().to_lowercase()).filter(|a| !a.is_empty()),
        name: s
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_string),
    };
    let out: Vec<Item> = parsed
        .iter()
        .flat_map(|a| match a {
            MailAddr::Single(s) => vec![item(s)],
            MailAddr::Group(g) => g.addrs.iter().map(item).collect(),
        })
        .collect();
    if out.is_empty() {
        return Err("holds no address".to_string());
    }
    Ok(out)
}

fn read_item(reader: FieldReader, item: &Item, scalar: &str) -> Option<String> {
    match reader {
        FieldReader::Address => item.addr.clone(),
        FieldReader::Domain => item
            .addr
            .as_deref()
            .and_then(|a| a.rsplit_once('@'))
            .map(|(_, d)| d.to_string())
            .filter(|d| !d.is_empty()),
        FieldReader::DisplayName => item.name.clone(),
        FieldReader::Value => Some(scalar.to_string()),
    }
}

/// Fold every model atom that carries a projected atom's identity value into
/// it, then put the projected atoms in the entity set. Inert with no sources.
///
/// Projected atoms go FIRST: `build_name_index` lets a later atom win a name,
/// and a name a model atom answers to must not be taken by a header's.
pub(super) fn fold_source_atoms(
    entities: &mut Vec<Entity>,
    name_index: &mut HashMap<String, AtomId>,
    mut sources: Vec<Entity>,
    policy: &ResolutionPolicy<'_>,
) -> SourceFoldReport {
    let mut report = SourceFoldReport {
        projected: sources.len(),
        ..Default::default()
    };
    if sources.is_empty() {
        return report;
    }
    let identity = policy.index().effective_identity_policy();
    let strict: Vec<Box<dyn MergeSignalCheck>> = vec![Box::new(ExternalIdSignal {
        keys_by_type: identity.identity.clone(),
    })];
    let projected: HashSet<AtomId> = sources.iter().map(|e| e.id.clone()).collect();
    let candidates: Vec<Entity> = sources.iter().chain(entities.iter()).cloned().collect();
    let outcome = reconcile_with_signals(
        candidates,
        &ReconciliationPolicy {
            identity,
            ..Default::default()
        },
        &strict,
    );
    let mut members: HashMap<AtomId, Vec<AtomId>> = HashMap::new();
    for cluster in outcome
        .entities
        .iter()
        .filter(|c| c.source_atom_ids.len() > 1)
    {
        let (ours, model): (Vec<&AtomId>, Vec<&AtomId>) = cluster
            .source_atom_ids
            .iter()
            .partition(|id| projected.contains(*id));
        match ours.as_slice() {
            [p] => {
                let mut model: Vec<AtomId> = model.into_iter().cloned().collect();
                model.sort();
                members.insert((*p).clone(), model);
            }
            [] => debug!(
                name = %cluster.canonical_name,
                "atlas/resolution sources: model atoms share an identity value no document \
                 field carries; left to `enrich reconcile`"
            ),
            // Distinct folded values by construction, so this is a defect.
            many => warn!(
                atoms = ?many,
                "atlas/resolution sources: projected atoms share one identity cluster; none folded"
            ),
        }
    }
    let at: HashMap<AtomId, usize> = entities
        .iter()
        .enumerate()
        .map(|(i, e)| (e.id.clone(), i))
        .collect();
    let mut folded: HashSet<AtomId> = HashSet::new();
    for p in sources.iter_mut() {
        let ty = p.entity_type.as_str_repr().to_string();
        // No document named it, so a model atom folded in may: a read name,
        // then a model's, then the key. A contact list of bare addresses is
        // not one a person reads, and the prototype this ports named it so.
        let named_by_key = p.canonical_name
            == key_name(
                &p.attributes,
                policy
                    .index()
                    .effective_identity(&ty)
                    .iter()
                    .map(String::as_str),
            );
        let mut model_name: Option<(f32, String)> = None;
        for id in members.remove(&p.id).unwrap_or_default() {
            // Every non-projected id the reconciler saw came from `entities`.
            let Some(m) = at.get(&id).map(|&i| &entities[i]) else {
                warn!(
                    model = id.as_str(),
                    "atlas/resolution sources: clustered atom is not in the entity set"
                );
                continue;
            };
            let (pid, mid) = (p.id.as_str(), m.id.as_str());
            if let Err(reason) = merge_permitted(
                policy,
                MergeEvidence::Exact,
                p.entity_type.as_str_repr(),
                &p.canonical_name,
                &p.attributes,
                m.entity_type.as_str_repr(),
                &m.canonical_name,
                &m.attributes,
            ) {
                debug!(%reason, pid, mid, "atlas/resolution sources: fold refused");
                *report.refused.entry(ty.clone()).or_default() += 1;
                continue;
            }
            debug!(pid, mid, name = %m.canonical_name, "atlas/resolution sources: folded");
            // The most salient model atom names it; ids arrive sorted, so a
            // tie goes to the first.
            if named_by_key
                && !m.canonical_name.trim().is_empty()
                && model_name.as_ref().is_none_or(|(s, _)| m.salience > *s)
            {
                model_name = Some((m.salience, m.canonical_name.clone()));
            }
            merge_into_existing(p, &sketch_of(m), name_index);
            for v in name_index.values_mut().filter(|v| **v == m.id) {
                *v = p.id.clone();
            }
            p.salience = p.salience.max(m.salience);
            folded.insert(m.id.clone());
            *report.merged.entry(ty.clone()).or_default() += 1;
        }
        if let Some((_, name)) = model_name {
            debug!(
                pid = p.id.as_str(),
                key = %p.canonical_name,
                %name,
                "atlas/resolution sources: named by its model atom"
            );
            let key = std::mem::replace(&mut p.canonical_name, name);
            p.aliases
                .retain(|a| !a.eq_ignore_ascii_case(&p.canonical_name));
            if !p.aliases.iter().any(|a| a.eq_ignore_ascii_case(&key)) {
                p.aliases.push(key);
            }
            *report.named.entry(ty.clone()).or_default() += 1;
        }
        for n in std::iter::once(&p.canonical_name).chain(&p.aliases) {
            name_index.entry(fold(n)).or_insert_with(|| p.id.clone());
        }
    }
    entities.retain(|e| !folded.contains(&e.id));
    sources.append(entities);
    *entities = sources;
    info!(
        projected = report.projected,
        merged = report.merged.values().sum::<usize>(),
        refused = report.refused.values().sum::<usize>(),
        "atlas/resolution sources: document-field atoms folded with model atoms"
    );
    report
}

/// A model atom as the mention `merge_into_existing` folds in.
fn sketch_of(m: &Entity) -> EntitySketch {
    EntitySketch {
        canonical_name: m.canonical_name.clone(),
        aliases: m.aliases.clone(),
        entity_type: m.entity_type.clone(),
        description: m.description.clone(),
        defining_quote: m.defining_quote.clone(),
        anchor: String::new(),
        attributes: m.attributes.clone(),
    }
}

/// The next free sequential entity index. `len() + 1` is not it once a fold
/// removed atoms (typo dedup, the source fold) or an atom holds a content-hash
/// id (a projected one), and reusing a live id overwrites an atom.
pub fn next_entity_index(entities: &[Entity]) -> usize {
    entities
        .iter()
        .filter_map(|e| e.id.as_str().strip_prefix("entity-"))
        // A content-hash id is 16 hex chars; a sequential one is shorter.
        .filter(|n| n.len() < 16 && n.bytes().all(|b| b.is_ascii_digit()))
        .filter_map(|n| n.parse::<usize>().ok())
        .max()
        .unwrap_or(0)
        + 1
}
