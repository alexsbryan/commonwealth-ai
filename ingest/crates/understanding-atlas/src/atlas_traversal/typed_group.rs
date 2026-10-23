// SPDX-License-Identifier: AGPL-3.0-or-later
//! The typed query's groups, split from [`super::typed`]: ONE grouper, which
//! a `tally` counts and a `sum` adds up over. An atom missing the group's
//! attribute is grouped under `(unset)` rather than dropped, and a ref's
//! group is the record it holds (keyed by that record's id, shown by its
//! name), so two orders naming one supplier land in one group.

use std::collections::BTreeMap;

use serde_json::Value;

use super::typed::ENUMERATE_MAX;
use super::typed_check::{Group, SUBJECT};
use super::typed_match::{Atom, Judge, Unsummed, Via};

const UNSET: &str = "(unset)";

/// One group: what the answer calls it, and its atoms.
struct Bucket<'a> {
    shown: String,
    atoms: Vec<Atom<'a>>,
}

/// The matched atoms per value of `by`, in the order a reader scans them
/// (by shown name, then by the value itself); `None` puts them in one group.
fn buckets<'a>(
    judge: &Judge<'a>,
    matched: &[(Atom<'a>, Vec<Via<'a>>)],
    by: Option<&Group<'_>>,
) -> Vec<Bucket<'a>> {
    let mut grouped: BTreeMap<(String, String), Vec<Atom<'a>>> = BTreeMap::new();
    for (atom, _) in matched {
        let key = match by {
            Some(g) => group_value(*atom, g),
            None => String::new(),
        };
        // A ref is shown by the record it holds; one holding a name no
        // record carries (or unset) is shown as it is held, not dropped.
        let name = match by {
            Some(g) if g.is_ref => match judge.record_name(&key) {
                Some(n) => n.to_string(),
                None => {
                    tracing::trace!(
                        atom = atom.id(),
                        by = g.attr,
                        held = key.as_str(),
                        "atlas traversal: group ref names no record; shown as held"
                    );
                    key.clone()
                }
            },
            _ => key.clone(),
        };
        grouped.entry((name, key)).or_default().push(*atom);
    }
    let mut out: Vec<Bucket<'a>> = Vec::new();
    let names: Vec<&String> = grouped.keys().map(|(n, _)| n).collect();
    let shared = |n: &String| names.iter().filter(|m| **m == n).count() > 1;
    for ((name, key), atoms) in &grouped {
        // Two records one name: the id tells them apart.
        let shown = if shared(name) {
            format!("{name} ({key})")
        } else {
            name.clone()
        };
        out.push(Bucket {
            shown,
            atoms: atoms.clone(),
        });
    }
    tracing::debug!(
        by = by.map(|g| g.attr),
        is_ref = by.is_some_and(|g| g.is_ref),
        atoms = matched.len(),
        groups = out.len(),
        "atlas traversal: typed query grouped atoms"
    );
    out
}

/// The value an atom is grouped by: the attribute as held (a ref's atom id or
/// written name), a claim's `subject` when it holds none, else `(unset)`.
fn group_value(atom: Atom<'_>, g: &Group<'_>) -> String {
    match atom.attributes().get(g.attr) {
        Some(Value::String(s)) => match s.trim() {
            "" => UNSET.to_string(),
            t => t.to_string(),
        },
        Some(Value::Null) => UNSET.to_string(),
        None if g.attr == SUBJECT => match atom.held(SUBJECT).as_slice() {
            [] => UNSET.to_string(),
            held => held.join(", "),
        },
        None => UNSET.to_string(),
        Some(v) => v.to_string(),
    }
}

/// The tally headline: how many matched atoms per group.
pub(super) fn tally_headline(
    judge: &Judge<'_>,
    matched: &[(Atom<'_>, Vec<Via<'_>>)],
    over: &Group<'_>,
    label: &str,
) -> String {
    let breakdown = buckets(judge, matched, Some(over))
        .iter()
        .map(|b| format!("{}: {}", b.shown, b.atoms.len()))
        .collect::<Vec<_>>()
        .join(", ");
    tracing::debug!(
        over = over.attr,
        total = matched.len(),
        "atlas traversal: aggregate over declared attribute"
    );
    format!("{} {label} by {} — {breakdown}", matched.len(), over.attr)
}

/// The sum headline — each group's total, how many atoms it adds, and how
/// many it could not — and the note naming every atom it could not add, by
/// id and why. An atom with no single readable value is never counted as 0.
pub(super) fn sum_headline(
    judge: &Judge<'_>,
    matched: &[(Atom<'_>, Vec<Via<'_>>)],
    over: &str,
    by: Option<&Group<'_>>,
    label: &str,
) -> (String, Vec<String>) {
    let mut unsummed: Vec<(String, Unsummed)> = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    for b in buckets(judge, matched, by) {
        let (mut total, mut added, mut unjudged) = (0.0_f64, 0usize, 0usize);
        for atom in &b.atoms {
            match judge.one_number(*atom, over) {
                Ok(x) => {
                    total += x;
                    added += 1;
                }
                Err(why) => {
                    tracing::debug!(
                        atom = atom.id(),
                        over,
                        why = why.reason(),
                        "atlas traversal: sum cannot add an atom's value"
                    );
                    unjudged += 1;
                    unsummed.push((atom.id().to_string(), why));
                }
            }
        }
        let value = match (added, unjudged) {
            (0, u) => format!("nothing summed ({u} unjudged)"),
            (n, 0) => format!("{} ({n})", shown_total(total)),
            (n, u) => format!("{} ({n}; {u} unjudged)", shown_total(total)),
        };
        parts.push(match by {
            Some(_) => format!("{}: {value}", b.shown),
            None => value,
        });
    }
    tracing::debug!(
        over,
        by = by.map(|g| g.attr),
        groups = parts.len(),
        unsummed = unsummed.len(),
        "atlas traversal: sum over declared attribute"
    );
    let breakdown = if parts.is_empty() {
        "no atom matches".to_string()
    } else {
        parts.join(", ")
    };
    let headline = match by {
        Some(g) => format!(
            "{} {label}, {over} summed by {} — {breakdown}",
            matched.len(),
            g.attr
        ),
        None => format!("{} {label}, {over} summed — {breakdown}", matched.len()),
    };
    let mut notes = Vec::new();
    if !unsummed.is_empty() {
        let mut ids: Vec<String> = unsummed
            .iter()
            .take(ENUMERATE_MAX)
            .map(|(id, why)| format!("{id} ({})", why.reason()))
            .collect();
        if unsummed.len() > ENUMERATE_MAX {
            ids.push(format!("and {} more", unsummed.len() - ENUMERATE_MAX));
        }
        notes.push(format!(
            "{} matching {label} atom(s) carry no single readable {over}, so were not summed: {}.",
            unsummed.len(),
            ids.join(", ")
        ));
    }
    (headline, notes)
}

/// A total as the answer prints it: rounded to a millionth, so summing
/// 0.1 and 0.2 reads 0.3, and a whole total prints without a decimal point.
fn shown_total(x: f64) -> String {
    let r = (x * 1e6).round() / 1e6 + 0.0;
    format!("{r}")
}
