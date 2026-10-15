// SPDX-License-Identifier: AGPL-3.0-or-later
//! A hit's catalogue entry for `svrn search` (OICP v0.6 §2.16): who, when,
//! what and where, from the document's record when the host merged one, or
//! from its declared metadata when it did not; then the fields a resolver
//! supplied, and each identifier that did not resolve, named.

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use sovereign_contracts::oicp::record::DECLARED;
use sovereign_contracts::oicp::{evidence::Document, LookupOutcome};

/// The lines printed under a hit for `d`: none when it declares nothing a
/// catalogue entry is made of.
pub(super) fn lines(d: &Document) -> Vec<String> {
    let csl = match (&d.record, &d.metadata) {
        (Some(r), _) => &r.csl,
        (None, Some(Value::Object(m))) => m,
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    if let Some(entry) = entry(csl) {
        out.push(entry);
    }
    let Some(r) = &d.record else { return out };
    let mut from: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, origin) in r.origins.iter().filter(|(_, o)| o.as_str() != DECLARED) {
        *from.entry(origin.as_str()).or_default() += 1;
    }
    if !from.is_empty() {
        let named: Vec<String> = from.iter().map(|(o, n)| format!("{n} from {o}")).collect();
        out.push(format!("fields: {}, the rest declared", named.join(", ")));
    }
    for l in &r.lookups {
        let said = match l.outcome {
            LookupOutcome::Resolved | LookupOutcome::NotAllowed => continue,
            LookupOutcome::Unresolved => "did not resolve",
            LookupOutcome::Unreachable => "was not answered",
        };
        out.push(format!(
            "{} {} {said} ({})",
            l.identifier, l.value, l.resolver
        ));
    }
    out
}

/// `Author (year). Title. Container.`, from what `csl` holds of it.
fn entry(csl: &Map<String, Value>) -> Option<String> {
    let title = csl.get("title").and_then(Value::as_str);
    let names: Vec<&str> = csl
        .get("author")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|n| n.get("family").or_else(|| n.get("literal"))?.as_str())
                .collect()
        })
        .unwrap_or_default();
    let who = match names.as_slice() {
        [] => None,
        [one] => Some(one.to_string()),
        [a, b] => Some(format!("{a} & {b}")),
        [first, ..] => Some(format!("{first} et al.")),
    };
    let year = csl.get("issued").and_then(|i| {
        i.pointer("/date-parts/0/0")
            .map(Value::to_string)
            .or_else(|| i.get("raw").and_then(Value::as_str).map(str::to_string))
    });
    let place = csl
        .get("container-title")
        .or_else(|| csl.get("publisher"))
        .and_then(Value::as_str);
    if who.is_none() && title.is_none() {
        return None;
    }
    let mut s = who.unwrap_or_default();
    if let Some(y) = year {
        s = format!("{s} ({y})").trim_start().to_string();
    }
    for part in [title, place].into_iter().flatten() {
        if !s.is_empty() {
            s.push_str(if s.ends_with('.') { " " } else { ". " });
        }
        s.push_str(part);
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(record: Option<Value>, metadata: Option<Value>) -> Document {
        serde_json::from_value(serde_json::json!({
            "text_sha256": "00", "extractor": "plain_text@test", "source": {"id": "x"},
            "record": record, "metadata": metadata,
        }))
        .unwrap()
    }

    #[test]
    fn a_record_prints_its_entry_where_its_fields_came_from_and_what_did_not_resolve() {
        let d = document(
            Some(serde_json::json!({
                "csl": {"title": "A Title the Recipe Declares", "container-title": "Scientific Data",
                        "author": [{"family": "Wilkinson", "given": "Mark D."}, {"family": "Dumontier"},
                                   {"family": "Aalbersberg"}],
                        "issued": {"date-parts": [[2016, 3, 15]]}, "DOI": "10.1038/sdata.2016.18",
                        "ISBN": "9780999999990"},
                "origins": {"title": "declared", "container-title": "crossref", "author": "crossref",
                            "issued": "crossref", "DOI": "declared", "ISBN": "declared"},
                "lookups": [
                    {"identifier": "DOI", "value": "10.1038/sdata.2016.18", "resolver": "crossref", "outcome": "resolved"},
                    {"identifier": "ISBN", "value": "9780999999990", "resolver": "open_library", "outcome": "unresolved"}]
            })),
            None,
        );
        assert_eq!(
            lines(&d),
            vec![
                "Wilkinson et al. (2016). A Title the Recipe Declares. Scientific Data",
                "fields: 3 from crossref, the rest declared",
                "ISBN 9780999999990 did not resolve (open_library)",
            ]
        );
    }

    #[test]
    fn declared_metadata_alone_prints_its_entry_and_nothing_else() {
        let d = document(
            None,
            Some(serde_json::json!({"title": "Notes", "author": [{"literal": "Okafor"}]})),
        );
        assert_eq!(lines(&d), vec!["Okafor. Notes"]);
        assert!(lines(&document(None, Some(serde_json::json!({"DOI": "10.1/x"})))).is_empty());
    }
}
