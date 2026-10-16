// SPDX-License-Identifier: AGPL-3.0-or-later
//! The window oracle (module doc of `layer_contract_tests`): what a window
//! read's prompt shows, read back, and the answer the fixture rules give it.

use super::*;

/// One document of a window read's prompt: its label (`d1`), the lines of
/// its head (declared facts, `Named here:`), and the lines of its body.
pub(super) struct Shown<'a> {
    pub(super) label: &'a str,
    pub(super) head: Vec<&'a str>,
    pub(super) body: Vec<&'a str>,
}

/// The documents a window read shows, in the order it shows them.
pub(super) fn shown_documents(user: &str) -> Vec<Shown<'_>> {
    let documents = user
        .split_once("\nDocuments:\n")
        .and_then(|(_, rest)| rest.rsplit_once("\n\nReturn JSON:"))
        .map_or("", |(documents, _)| documents);
    let mut out: Vec<Shown<'_>> = Vec::new();
    for line in documents.lines() {
        match line
            .strip_prefix("=== ")
            .and_then(|l| l.strip_suffix(" ==="))
        {
            Some(label) => out.push(Shown {
                label,
                head: Vec::new(),
                body: Vec::new(),
            }),
            None => match out.last_mut() {
                Some(d) if d.body.is_empty() && !line.is_empty() => d.head.push(line),
                Some(d) => d.body.push(line),
                None => {}
            },
        }
    }
    out
}

/// The lines every window read of `run` showed as a document's own, of the
/// document whose head states `fact` (`id: q1@birchhall.example`) or of
/// every document: what a claim may cite. A pointer to a quoted document is
/// none.
pub(super) fn own_lines<'a>(run: &'a Run, fact: Option<&str>) -> Vec<&'a str> {
    run.prompts
        .iter()
        .filter(|p| p.phase_id.as_deref() == Some("document_window"))
        .flat_map(|p| shown_documents(&p.user))
        .filter(|d| fact.is_none_or(|f| d.head.contains(&f)))
        .flat_map(|d| d.body)
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("[quoted from"))
        .collect()
}

/// A window read's ontology as its prompt lists it: the first statement
/// kind with its subject type, and each kind's and type's fields to write as
/// (name, family, closed values).
pub(super) type Listed<'a> = (
    Option<(&'a str, &'a str)>,
    BTreeMap<&'a str, Vec<(&'a str, &'a str, Vec<&'a str>)>>,
);

pub(super) fn listed_fields(user: &str) -> Listed<'_> {
    let ontology = user
        .split_once("\nOntology:\n")
        .and_then(|(_, rest)| rest.split_once("\n\nHow to read:"))
        .map_or("", |(o, _)| o);
    let mut first: Option<(&str, &str)> = None;
    let mut fields: BTreeMap<&str, Vec<(&str, &str, Vec<&str>)>> = BTreeMap::new();
    let mut owner = "";
    for line in ontology.lines() {
        if let Some(rest) = line.strip_prefix("- ") {
            let head = rest.split_once(": ").map_or(rest, |(h, _)| h);
            owner = match head.split_once(" (a statement about one ") {
                Some((kind, subject)) => {
                    first.get_or_insert((kind, subject.trim_end_matches(')')));
                    kind
                }
                None => head,
            };
        } else if let Some((name, rest)) =
            line.strip_prefix("  * ").and_then(|a| a.split_once(" ("))
        {
            let family = rest.split_once(')').map_or(rest, |(f, _)| f);
            let values = rest
                .split_once(" One of: ")
                .map(|(_, v)| v.trim_end_matches('.').split(" | ").collect())
                .unwrap_or_default();
            fields
                .entry(owner)
                .or_default()
                .push((name, family, values));
        }
    }
    (first, fields)
}

/// The window oracle's answer (module doc): for each document, one
/// particular of the first statement kind's subject type, and a claim of
/// that kind for each statement, a line of its own of six words or more
/// that is no quote, pointer or placeholder and continues no statement's
/// sentence. A closed field takes its first value (`Spread`: one picked from
/// the bytes of the statement and of its document's declared facts); an open one the statement's
/// first word; a number the statement's first token holding a digit, else
/// none; a reference the first record of its type named beside the
/// document, else a party the fixture knows that the document names, else
/// the statement's first word. Every name is read from the prompt.
pub(super) fn window_answer(user: &str, choose: Choose, parties: Parties) -> Value {
    let (first, fields) = listed_fields(user);
    let Some((kind, subject)) = first else {
        return json!({"particulars": [], "claims": []});
    };
    let (mut particulars, mut claims) = (Vec::new(), Vec::new());
    for d in shown_documents(user) {
        let text = d.body.join("\n");
        let named = |of: &str| {
            d.head
                .iter()
                .filter_map(|h| h.strip_prefix("Named here: "))
                .flat_map(|h| h.split("; "))
                .filter_map(|c| c.strip_prefix(&format!("{of} ")))
                .map(|c| c.split([' ', ',']).next().unwrap_or(c).to_string())
                .next()
        };
        let first_word = |quote: &str| {
            let w = quote.split_whitespace().next().unwrap_or("");
            w.trim_matches(|c: char| !c.is_alphanumeric()).to_string()
        };
        let value = |family: &str, values: &[&str], quote: &str| -> Option<Value> {
            if !values.is_empty() {
                let facts = d.head.iter().flat_map(|l| l.bytes());
                let sum: usize = quote.bytes().chain(facts).map(usize::from).sum();
                let at = if choose == Choose::Spread {
                    sum % values.len()
                } else {
                    0
                };
                return Some(json!(values[at]));
            }
            match family {
                "text" | "a time" => Some(json!(first_word(quote))),
                f if f.starts_with("a number") => quote
                    .split_whitespace()
                    .find(|w| w.chars().any(|c| c.is_ascii_digit()))
                    .map(|w| json!(w.trim_end_matches(|c: char| !c.is_ascii_digit()))),
                f => {
                    let of = f.strip_prefix("a ").unwrap_or(f);
                    let party = || {
                        parties
                            .iter()
                            .find(|(t, name, _)| *t == of && text.contains(name))
                            .map(|(_, name, _)| name.to_string())
                    };
                    Some(json!(named(of)
                        .or_else(party)
                        .unwrap_or_else(|| first_word(quote))))
                }
            }
        };
        let mut statements: Vec<&str> = Vec::new();
        let mut open = false;
        for line in d.body.iter().map(|l| l.trim()) {
            let statement =
                line.split_whitespace().count() >= 6 && !line.starts_with(['>', '[', '(']);
            if statement && !open {
                statements.push(line);
            }
            open = statement && !line.ends_with(['.', '!', '?']);
        }
        let Some(opening) = statements.first() else {
            continue;
        };
        let about = format!("p{}", d.label.trim_start_matches('d'));
        let read = |owner: &str, quote: &str, cited: bool| -> Map<String, Value> {
            let mut out = Map::new();
            for (name, family, values) in fields.get(owner).into_iter().flatten() {
                if let Some(v) = value(family, values, quote) {
                    let v = if cited {
                        json!({"value": v, "quote": quote})
                    } else {
                        v
                    };
                    out.insert(name.to_string(), v);
                }
            }
            out
        };
        let name: Vec<&str> = opening.split_whitespace().take(4).collect();
        particulars.push(
            json!({"ref": about, "type": subject, "name": name.join(" "),
            "fields": read(subject, opening, true)}),
        );
        for quote in statements {
            claims.push(json!({"kind": kind, "about": about, "document": d.label,
                "quote": quote, "fields": read(kind, quote, false)}));
        }
    }
    json!({"particulars": particulars, "claims": claims})
}
