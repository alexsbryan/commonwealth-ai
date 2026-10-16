// SPDX-License-Identifier: AGPL-3.0-or-later
//! What each statement's reader read its subject to be (`Described`), gathered
//! from the claims the reader wrote about it.

use std::collections::{BTreeSet, HashMap};

use serde_json::{Map, Value};

use super::Described;
use crate::enrichment::pipeline::document_read::{
    other_names_in, window_subject_name, OTHER_NAMES_ATTRIBUTE,
};

/// What a window read named a statement by: its subject's name, then the
/// other names a document gives it, and the other names of every thing the
/// statement names (`OTHER_NAMES_ATTRIBUTE`). None when it names nothing.
#[derive(Debug, Default)]
pub(super) struct ReadNames {
    pub(super) names: Vec<String>,
    pub(super) others: Map<String, Value>,
}

impl ReadNames {
    pub(super) fn of(attributes: &Map<String, Value>) -> Option<Self> {
        let others = attributes
            .get(OTHER_NAMES_ATTRIBUTE)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let names: Vec<String> = window_subject_name(attributes)
            .into_iter()
            .flat_map(|name| std::iter::once(name).chain(other_names_in(&others, name)))
            .map(str::to_string)
            .collect();
        (!names.is_empty() || !others.is_empty()).then_some(Self { names, others })
    }
}

/// Each claim's description of its subject: the names its window read gave it
/// (`names_of`: its name, then the other names a document gives it), and its
/// supported values of the type's attributes outside `carried` (the keys and
/// necessary attributes, which travel as keys and reads), each with the other
/// names the read gave the thing it names: shown, never compared. A window read's
/// claims about one chapter subject all get the union over that subject, so
/// a question about any of them shows all the window read of it, whichever of
/// its documents RESOLVE meets first.
pub(super) fn of_claims(
    fields_of: &HashMap<usize, Map<String, Value>>,
    names_of: &HashMap<usize, ReadNames>,
    chapter_subject_of: &HashMap<usize, String>,
    carried: &BTreeSet<&str>,
) -> HashMap<usize, Described> {
    let mut out = HashMap::<usize, Described>::new();
    for (&i, read) in names_of {
        out.entry(i).or_default().names.extend(read.names.iter().cloned());
    }
    for &i in chapter_subject_of.keys() {
        out.entry(i).or_default();
    }
    for (&i, fields) in fields_of {
        let d = out.entry(i).or_default();
        for (attr, value) in fields.iter().filter(|(a, _)| !carried.contains(a.as_str())) {
            let value = match value {
                Value::String(text) => text.clone(),
                Value::Number(number) => number.to_string(),
                _ => continue,
            };
            let others = names_of
                .get(&i)
                .map(|r| other_names_in(&r.others, &value))
                .unwrap_or_default();
            let shown = d.values.entry(attr.clone()).or_default();
            shown.extend(others.into_iter().map(str::to_string));
            shown.insert(value);
        }
    }
    let mut of_subject = HashMap::<&str, Described>::new();
    for (i, d) in &out {
        if let Some(subject) = chapter_subject_of.get(i) {
            of_subject.entry(subject.as_str()).or_default().extend(d);
        }
    }
    for (i, d) in out.iter_mut() {
        if let Some(whole) = chapter_subject_of
            .get(i)
            .and_then(|s| of_subject.get(s.as_str()))
        {
            *d = whole.clone();
        }
    }
    out
}

#[cfg(test)]
#[path = "described_tests.rs"]
mod tests;
