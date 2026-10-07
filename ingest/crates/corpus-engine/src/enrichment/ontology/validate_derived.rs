// SPDX-License-Identifier: AGPL-3.0-or-later
//! `recipe validate` for derived attributes (`ontology::derived`): every id
//! declared once, every step a keyword, a declared id, a `ref` attribute or a
//! field a metadata source reads, every filter a declared set, every derived
//! attribute a `ref` filled by a path or fold, no cycle, and nothing derived
//! before RESOLVE that reads what RESOLVE makes. Each derived attribute is
//! printed in words, with the side of RESOLVE it runs on.

use std::collections::{BTreeMap, BTreeSet};

use super::derived::{DerivedPolicy, Named, PathExpr, DOCUMENT, SUBJECT};
use super::{AttrFamily, OntologyPolicies, SourceDecl, TypeIndex, TypeKind};
use crate::enrichment::atlas::resolution_records::decides;

pub(super) fn check(p: &OntologyPolicies, errors: &mut Vec<String>, notes: &mut Vec<String>) {
    let d = &p.derivation.derived;
    let derived: Vec<(&str, &str, &str, &AttrFamily)> = p
        .shape
        .types
        .iter()
        .flat_map(|t| {
            t.attributes.iter().filter_map(move |a| {
                Some((
                    t.name.as_str(),
                    a.name.as_str(),
                    a.derived.as_deref()?,
                    &a.family,
                ))
            })
        })
        .collect();
    if d.is_empty() && derived.is_empty() {
        return;
    }
    let index = TypeIndex::from_policies(p);
    let mut seen = BTreeSet::new();
    let ids = d
        .paths
        .iter()
        .map(|x| &x.id)
        .chain(d.sets.iter().map(|x| &x.id))
        .chain(d.folds.iter().map(|x| &x.id));
    for id in ids {
        if !seen.insert(id.as_str()) {
            errors.push(format!(
                "derived: `{id}` is declared twice among paths, sets and folds"
            ));
        }
        if id == SUBJECT || id == DOCUMENT {
            errors.push(format!("derived: `{id}` is a step keyword, not an id"));
        }
    }
    if p.shape.types.iter().any(|t| t.name == DOCUMENT) {
        errors.push(format!("ontology type `{DOCUMENT}` collides with the path step and set type of that name; rename it"));
    }
    let attrs: BTreeMap<&str, Vec<&AttrFamily>> = p
        .shape
        .types
        .iter()
        .flat_map(|t| &t.attributes)
        .fold(BTreeMap::new(), |mut m, a| {
            m.entry(a.name.as_str())
                .or_insert_with(Vec::new)
                .push(&a.family);
            m
        });
    let fields: BTreeSet<&str> = p
        .shape
        .types
        .iter()
        .filter_map(|t| match &t.source {
            Some(SourceDecl::Metadata(s)) => Some(s.metadata.iter().map(String::as_str)),
            _ => None,
        })
        .flatten()
        .collect();
    for id in d
        .paths
        .iter()
        .map(|x| &x.id)
        .chain(d.folds.iter().map(|x| &x.id))
    {
        let exprs = match d.exprs(id) {
            Ok(e) => e,
            Err(e) => {
                errors.push(format!("derived: {e}"));
                continue;
            }
        };
        if exprs.is_empty() {
            errors.push(format!("derived: fold `{id}` has an empty `from`"));
        }
        for e in &exprs {
            for set in e.sets() {
                if !matches!(d.get(set), Some(Named::SetDecl(_))) {
                    errors.push(format!(
                        "derived: `{id}` filters by `[{set}]`, which is no declared set"
                    ));
                }
            }
            for (name, inverse) in e.steps() {
                if let Some(why) = step_error(d, &attrs, &fields, name, inverse) {
                    errors.push(format!("derived: `{id}`: {why}"));
                }
            }
        }
    }
    for s in &d.sets {
        if s.of == DOCUMENT {
            continue;
        }
        if !index.contains(&s.of) {
            errors.push(format!(
                "derived: set `{}` ranges over `{}`, which is no declared type",
                s.id, s.of
            ));
            continue;
        }
        let has: BTreeSet<&str> = index
            .effective_attributes(&s.of)
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        for attr in s.conditions.keys().filter(|a| !has.contains(a.as_str())) {
            errors.push(format!(
                "derived: set `{}` reads `{attr}`, which `{}` does not declare",
                s.id, s.of
            ));
        }
    }
    for (t, a, id, family) in &derived {
        if !matches!(d.get(id), Some(Named::PathDecl(_) | Named::FoldDecl(_))) {
            errors.push(format!("ontology type `{t}`: attribute `{a}` is derived from `{id}`, which is no declared path or fold"));
        }
        if !matches!(family, AttrFamily::Ref { .. }) {
            errors.push(format!("ontology type `{t}`: derived attribute `{a}` must be a `ref`; a path reaches particulars"));
        }
    }
    let order = match d.order(&p.shape.types) {
        Ok(o) => o,
        Err(e) => {
            errors.push(format!("derived: {e}"));
            return;
        }
    };
    let decided_subject = p.shape.types.iter().any(|c| {
        c.kind == TypeKind::Claim && c.subject.as_deref().is_some_and(|s| decides(&index, s))
    });
    let only_on_decided = |attr: &str| {
        let on: Vec<&str> = p
            .shape
            .types
            .iter()
            .filter(|t| t.attributes.iter().any(|a| a.name == attr))
            .map(|t| t.name.as_str())
            .collect();
        !on.is_empty() && on.iter().all(|t| decides(&index, t))
    };
    for (t, a, id) in order {
        let after = decides(&index, t);
        if !after {
            let reads = d.reads(id).unwrap_or_default();
            if let Some(r) = reads.iter().find(|r| only_on_decided(r)) {
                errors.push(format!("derived: `{t}.{a}` is derived before RESOLVE but reads `{r}`, which only RESOLVE's records carry"));
            }
            if decided_subject && walks_to_subject(d, id, &mut Vec::new()) {
                errors.push(format!("derived: `{t}.{a}` is derived before RESOLVE but steps to a claim's `subject`, which RESOLVE decides"));
            }
        }
        let how = match d.get(id) {
            Some(Named::FoldDecl(f)) => format!("fold `{id}` by `{}`", f.by.label()),
            _ => format!("path `{id}`, every value"),
        };
        let inputs: Vec<String> = d
            .exprs(id)
            .unwrap_or_default()
            .iter()
            .map(|e| match e {
                // a bare id reads as what it walks
                PathExpr::Step {
                    name,
                    inverse: false,
                } if matches!(d.get(name), Some(Named::PathDecl(_))) => {
                    let walk: Vec<String> = d
                        .exprs(name)
                        .unwrap_or_default()
                        .iter()
                        .map(PathExpr::describe)
                        .collect();
                    format!("`{name}` ({})", walk.join("; "))
                }
                e => e.describe(),
            })
            .collect();
        notes.push(format!(
            "derived: {t}.{a} ← {how}, {} RESOLVE: {}",
            if after { "after" } else { "before" },
            inputs.join("; then ")
        ));
    }
    let used: BTreeSet<String> = derived
        .iter()
        .flat_map(|(_, _, id, _)| reachable(d, id))
        .collect();
    for id in d
        .paths
        .iter()
        .map(|x| &x.id)
        .chain(d.folds.iter().map(|x| &x.id))
        .chain(d.sets.iter().map(|x| &x.id))
    {
        if !used.contains(id) {
            notes.push(format!(
                "derived: `{id}` is declared and no derived attribute uses it"
            ));
        }
    }
}

/// Why a step name does not resolve, or `None`.
fn step_error(
    d: &DerivedPolicy,
    attrs: &BTreeMap<&str, Vec<&AttrFamily>>,
    fields: &BTreeSet<&str>,
    name: &str,
    inverse: bool,
) -> Option<String> {
    let is_attr = attrs.contains_key(name);
    match (name, d.get(name)) {
        (SUBJECT, _) => None,
        (DOCUMENT, _) if inverse => Some("`^document` walks nowhere: a document is reached from a claim".into()),
        (DOCUMENT, _) => None,
        (_, Some(Named::SetDecl(_))) => Some(format!("`{name}` is a set; filter a step with `[{name}]`")),
        (_, Some(_)) if is_attr => Some(format!("`{name}` is both a declared id and an attribute; rename one")),
        (_, Some(_)) if inverse => Some(format!("`^{name}`: an id is not walked backwards")),
        (_, Some(_)) => None,
        _ if is_attr => attrs[name]
            .iter()
            .any(|f| matches!(f, AttrFamily::Ref { .. }))
            .then_some(())
            .map_or_else(|| Some(format!("`{name}` is no `ref` attribute; a step reaches particulars through one")), |()| None),
        _ if fields.contains(name) && inverse => Some(format!("`^{name}`: a document field is not walked backwards")),
        _ if fields.contains(name) => None,
        _ => Some(format!("`{name}` is no step keyword, declared id, `ref` attribute or field a metadata source reads")),
    }
}

/// Whether `id`, through the ids it names, steps forward to a claim's subject.
fn walks_to_subject(d: &DerivedPolicy, id: &str, stack: &mut Vec<String>) -> bool {
    if stack.iter().any(|s| s == id) {
        return false;
    }
    stack.push(id.to_string());
    let hit = d
        .exprs(id)
        .unwrap_or_default()
        .iter()
        .flat_map(PathExpr::steps)
        .any(|(n, inv)| {
            (n == SUBJECT && !inv)
                || (matches!(d.get(n), Some(Named::PathDecl(_) | Named::FoldDecl(_)))
                    && walks_to_subject(d, n, stack))
        });
    stack.pop();
    hit
}

/// `id` and every id and set it reaches.
fn reachable(d: &DerivedPolicy, id: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut todo = vec![id.to_string()];
    while let Some(x) = todo.pop() {
        if !out.insert(x.clone()) {
            continue;
        }
        for e in d.exprs(&x).unwrap_or_default() {
            todo.extend(e.sets().into_iter().map(str::to_string));
            todo.extend(
                e.steps()
                    .into_iter()
                    .filter(|(n, _)| d.get(n).is_some())
                    .map(|(n, _)| n.to_string()),
            );
        }
    }
    out
}
