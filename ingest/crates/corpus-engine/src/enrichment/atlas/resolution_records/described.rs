// SPDX-License-Identifier: AGPL-3.0-or-later
//! What each statement's reader read its subject to be (`Described`), gathered
//! from the claims the reader wrote about it.

use std::collections::{BTreeSet, HashMap};

use serde_json::{Map, Value};

use super::Described;

/// Each claim's description of its subject: the name its window read gave it
/// (`names_of`), and its supported values of the type's
/// attributes outside `carried` (the keys and necessary attributes, which
/// travel as keys and reads). A window read's
/// claims about one chapter subject all get the union over that subject, so
/// a question about any of them shows all the window read of it, whichever of
/// its documents RESOLVE meets first.
pub(super) fn of_claims(
    fields_of: &HashMap<usize, Map<String, Value>>,
    names_of: &HashMap<usize, String>,
    chapter_subject_of: &HashMap<usize, String>,
    carried: &BTreeSet<&str>,
) -> HashMap<usize, Described> {
    let mut out = HashMap::<usize, Described>::new();
    for (&i, name) in names_of {
        out.entry(i).or_default().names.insert(name.clone());
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
            d.values.entry(attr.clone()).or_default().insert(value);
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
