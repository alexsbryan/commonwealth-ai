//! Particulars the window's reader named that no statement of the window is
//! about. Each reaches RESOLVE as a statement of its own
//! (`DocumentReadParticular`), on every document whose own lines name it,
//! so a person only
//! ever spoken of is a record, decided across windows by RESOLVE like any
//! other. Failing input, named: saltgrass (2026-10-14), whose window reader
//! named nine characters, the killer among them, of which four became records:
//! the window envelope sent claims only. On every such document, not the
//! first only: a reference resolves within its own document, so an owner
//! named in the second of two emails held nothing when the person was sent
//! on the first (02-contracts n1, 2026-10-20). What a document states is read
//! in that document, as a field is (`own_entry`).

use std::collections::BTreeSet;

use serde_json::Value;
use tracing::{debug, trace};

use super::super::pick::Candidate;
use super::super::validation::field_evidence_exists;
use super::super::{DocumentReadField, DocumentReadOutcome, DocumentReadParticular};
use super::{text, Contract};
use crate::enrichment::atlas::SourceDocument;
use crate::enrichment::ontology::{
    AttrDecl, OntologyPolicies, OntologyTypeDecl, SourceDecl, TypeIndex,
};
use crate::enrichment::pipeline::types::ChapterInput;
use crate::enrichment::reconciliation::identity_signals::fold_identity_value;

/// Add every unstated particular of a statement subject type to the outcome
/// of each document whose own lines name it: by its name, or else by a
/// field's quote, and that document's first such line is its evidence. The
/// statements share the particular's chapter subject, so RESOLVE makes them
/// one record. One no own line names cites
/// nothing and is dropped, traced; one of a type no statement kind is about
/// is left to the references that name it, traced.
#[allow(clippy::too_many_arguments)]
pub(super) fn unstated(
    chapter: &ChapterInput,
    policies: &OntologyPolicies,
    index: &TypeIndex<'_>,
    contract: &Contract<'_>,
    particulars: &[Value],
    claims: &[Value],
    owns: &[String],
    outcomes: &mut [(DocumentReadOutcome, Vec<(String, Vec<Candidate>)>)],
) {
    let stated: BTreeSet<String> = claims.iter().map(|c| text(&c["about"])).collect();
    for p in particulars {
        let local = text(&p["ref"]);
        if local.is_empty() || stated.contains(&local) {
            continue;
        }
        let ty = text(&p["type"]);
        let name = text(&p["name"]);
        let Some(subject) = contract.subjects.iter().copied().find(|s| s.name == ty) else {
            debug!(chapter = %chapter.chapter_id, particular = %local, %ty, %name, "document_read/window: no statement kind is about this type; its references name it");
            continue;
        };
        // Its names (its name, then the other names a document gives it),
        // then its fields' quotes.
        let mut cues: Vec<String> = names_of(p);
        if let Some(fields) = p["fields"].as_object() {
            cues.extend(
                fields
                    .values()
                    .flat_map(field_entries)
                    .filter_map(|f| f.get("quote"))
                    .map(text),
            );
        }
        let found: Vec<(usize, String)> = owns
            .iter()
            .enumerate()
            .filter_map(|(k, own)| {
                cues.iter()
                    .filter(|c| !c.trim().is_empty())
                    .find_map(|c| line_naming(own, c))
                    .map(|line| (k, line))
            })
            .collect();
        if found.is_empty() {
            debug!(chapter = %chapter.chapter_id, particular = %local, %name, "document_read/window: no own line of the window names this particular; dropped");
            continue;
        }
        if found.len() > 1 {
            debug!(chapter = %chapter.chapter_id, particular = %local, %name, documents = found.len(), "document_read/window: an unstated particular sent on each document that names it");
        }
        for (k, evidence) in found {
            let document = &chapter.source_documents[k];
            let (outcome, named) = &mut outcomes[k];
            let subject_fields =
                particular_fields(policies, index, subject, p, document, named, particulars);
            trace!(document = %outcome.document_id, particular = %local, %name, %evidence, "document_read/window: an unstated particular sent as its own statement");
            let mut things = vec![(name.clone(), ty.clone())];
            things.extend(ref_values(
                super::subject_attrs(policies, index, &ty),
                &subject_fields,
            ));
            let other_names = other_names_for(
                particulars,
                index,
                things.iter().map(|(n, t)| (n.as_str(), t.as_str())),
            );
            outcome.particulars.push(DocumentReadParticular {
                subject_type: ty.clone(),
                subject_local_ref: format!("{}/{local}", chapter.chapter_id),
                subject_name: if name.is_empty() {
                    local.clone()
                } else {
                    name.clone()
                },
                evidence,
                subject_fields,
                other_names,
            });
        }
    }
}

/// The name of the particular whose `ref` label is `words`, when it is an `of`
/// (or `is_a` one) and has a name: the window prompt has the model label its
/// particulars and refer to them by label (`window_prompt.md`).
pub(super) fn label_name(
    particulars: &[Value],
    index: &TypeIndex<'_>,
    of: &str,
    words: &str,
    document: &SourceDocument,
) -> Option<String> {
    let name = particulars
        .iter()
        .find(|p| text(&p["ref"]) == words)
        .filter(|p| {
            let ty = text(&p["type"]);
            ty == of || index.is_a(&ty, of)
        })
        .map(|p| text(&p["name"]))
        .filter(|name| !name.is_empty())?;
    debug!(document = %document.key(), particular = %words, %name, "document_read/window: a ref label read as its particular's name");
    Some(name)
}

/// A particular's names: its `name`, then each of the `other_names` the
/// answer lists for it (a name a document says it also has, a former or a new
/// one), each once and none blank. A typed list: no name is ever split, so
/// "Input / Output" is one name.
pub(super) fn names_of(p: &Value) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let others = p["other_names"].as_array().into_iter().flatten().map(text);
    for name in std::iter::once(text(&p["name"])).chain(others) {
        if !name.trim().is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// The names of the thing `words` names: when it is the label, the name or
/// one of the other names of a particular of type `of` (or `is_a` one), that
/// particular's names (`names_of`), `words`' own first; else `words` alone.
pub(super) fn names_for(
    particulars: &[Value],
    index: &TypeIndex<'_>,
    of: &str,
    words: &str,
) -> Vec<String> {
    let want = fold_identity_value(words);
    let group = particulars
        .iter()
        .filter(|p| {
            let ty = text(&p["type"]);
            ty == of || index.is_a(&ty, of)
        })
        .map(|p| (text(&p["ref"]), names_of(p)))
        .find(|(label, names)| {
            label == words
                || names
                    .iter()
                    .any(|n| want.is_some() && fold_identity_value(n) == want)
        });
    let mut out = vec![words.to_string()];
    if let Some((_, names)) = group.filter(|(_, names)| names.len() > 1) {
        trace!(%words, names = names.len(), "document_read/window: a reference names a particular with other names");
        out.extend(names.into_iter().filter(|n| n != words));
    }
    out
}

/// For each of `wanted` (a thing a statement names: its subject's name and
/// type, each reference reading's value and the type it refers to) that is a
/// name of a particular of that type (or an `is_a` one) with other names,
/// those other names ([`OTHER_NAMES_ATTRIBUTE`](super::super::OTHER_NAMES_ATTRIBUTE)).
pub(super) fn other_names_for<'a>(
    particulars: &[Value],
    index: &TypeIndex<'_>,
    wanted: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> std::collections::BTreeMap<String, Vec<String>> {
    let groups: Vec<(String, Vec<String>)> = particulars
        .iter()
        .map(|p| (text(&p["type"]), names_of(p)))
        .filter(|(_, names)| names.len() > 1)
        .collect();
    let mut out = std::collections::BTreeMap::new();
    for (name, of) in wanted {
        let Some(want) = fold_identity_value(name) else {
            continue;
        };
        let same = |n: &String| fold_identity_value(n).as_deref() == Some(want.as_str());
        for (_, group) in groups
            .iter()
            .filter(|(ty, group)| (ty == of || index.is_a(ty, of)) && group.iter().any(same))
        {
            let others: &mut Vec<String> = out.entry(name.to_string()).or_default();
            for n in group.iter().filter(|n| !same(n)) {
                if !others.contains(n) {
                    others.push(n.clone());
                }
            }
        }
    }
    out
}

/// Each supported reference reading among `readings` as (its value, the type
/// it refers to), for the declarations `attrs`: what a statement names by
/// reference.
pub(super) fn ref_values<'a>(
    attrs: impl IntoIterator<Item = &'a crate::enrichment::ontology::AttrDecl>,
    readings: &std::collections::BTreeMap<String, super::super::DocumentReadField>,
) -> Vec<(String, String)> {
    attrs
        .into_iter()
        .filter_map(|a| match (&a.family, readings.get(&a.name)) {
            (
                crate::enrichment::ontology::AttrFamily::Ref { of },
                Some(super::super::DocumentReadField::Supported {
                    value: Value::String(v),
                    ..
                }),
            ) => Some((v.clone(), of.clone())),
            _ => None,
        })
        .collect()
}

/// A particular's declared fields as the window read them, each supported only
/// by what `document` holds; an identity its metadata source names fills an
/// unknown one. One reading for a claim's subject and a particular of its own.
pub(super) fn particular_fields(
    policies: &OntologyPolicies,
    index: &TypeIndex<'_>,
    subject: &OntologyTypeDecl,
    particular: &Value,
    document: &SourceDocument,
    named: &[(String, Vec<Candidate>)],
    particulars: &[Value],
) -> std::collections::BTreeMap<String, DocumentReadField> {
    super::subject_attrs(policies, index, &subject.name)
        .into_iter()
        .map(|a| {
            let entry = own_entry(&particular["fields"][&a.name], document);
            let (value, evidence) = match entry {
                Value::Object(o) if o.contains_key("value") => {
                    (o.get("value"), o.get("quote").map(text))
                }
                Value::Null => (None, None),
                other => (Some(other), None),
            };
            let cite = evidence.as_deref();
            let mut r = super::reading(a, value, cite, document, named, particulars, index);
            if matches!(r, DocumentReadField::Unknown { .. }) {
                r = sourced_identity(subject, a, &names_of(particular), index, named).unwrap_or(r);
            }
            (a.name.clone(), r)
        })
        .collect()
}

/// A field's entries: the window writes one `{value, quote}` per document that
/// states it (`window_prompt.md`), and an older answer one object.
fn field_entries(field: &Value) -> &[Value] {
    match field {
        Value::Array(entries) => entries,
        Value::Null => &[],
        one => std::slice::from_ref(one),
    }
}

/// The entry `document` states: the first whose quote is in it, else the first
/// whose value's words are, else the first written, which `reading` judges
/// against `document` as it judged a field written once. Failing input, named:
/// mailraw's price was written once per window (2,250, message 06), so the
/// 2,400 of messages 03 and 05 never reached the `agree` fold and its conflict
/// read as decided (runs/loop/ship/journeys-2, 2026-10-20).
fn own_entry<'v>(field: &'v Value, document: &SourceDocument) -> &'v Value {
    let entries = field_entries(field);
    let holds = |e: &Value, key: &str| {
        let words = text(&e[key]);
        !words.is_empty() && field_evidence_exists(document, &words)
    };
    if let Some(e) = entries.iter().find(|e| holds(e, "quote")) {
        trace!(document = %document.key(), entries = entries.len(), "document_read/window: a field's entry chosen by its quote in this document");
        return e;
    }
    if let Some(e) = entries.iter().find(|e| holds(e, "value")) {
        trace!(document = %document.key(), entries = entries.len(), "document_read/window: a field's entry chosen by its value's words in this document");
        return e;
    }
    match entries.first() {
        Some(e) => {
            trace!(document = %document.key(), entries = entries.len(), "document_read/window: no entry of a field is in this document; the first is judged");
            e
        }
        None => &Value::Null,
    }
}

/// An identity field of a subject its metadata source names, read from the
/// record this document's fields name by one of the particular's `names`
/// (`names_of`: its name, then its other names): the model names a person,
/// the source holds their address.
fn sourced_identity(
    subject: &OntologyTypeDecl,
    attr: &AttrDecl,
    names: &[String],
    index: &TypeIndex<'_>,
    named: &[(String, Vec<Candidate>)],
) -> Option<DocumentReadField> {
    if !matches!(subject.source, Some(SourceDecl::Metadata(_))) {
        return None;
    }
    let at = index
        .effective_identity(&subject.name)
        .iter()
        .position(|k| *k == attr.name)?;
    let wanted: Vec<String> = names
        .iter()
        .filter_map(|n| fold_identity_value(n))
        .collect();
    named
        .iter()
        .filter(|(t, _)| *t == subject.name)
        .flat_map(|(_, cs)| cs)
        .find_map(|c| match c {
            Candidate::Record {
                identity,
                names,
                scalar,
                ..
            } if names
                .iter()
                .chain(identity.iter())
                .any(|n| fold_identity_value(n).is_some_and(|n| wanted.contains(&n))) =>
            {
                Some(super::supported(
                    Value::String(identity.get(at)?.clone()),
                    scalar,
                ))
            }
            _ => None,
        })
}

/// The first line of `own` holding `cue`, case and whitespace aside.
fn line_naming(own: &str, cue: &str) -> Option<String> {
    let fold = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    let cue = fold(cue);
    own.lines()
        .find(|l| fold(l).contains(&cue))
        .map(|l| l.trim().to_string())
}
