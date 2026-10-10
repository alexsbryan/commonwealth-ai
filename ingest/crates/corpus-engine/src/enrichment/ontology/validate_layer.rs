// SPDX-License-Identifier: AGPL-3.0-or-later
//! `recipe validate`'s warnings about the ontology layer's default path
//! (campaign ontology-layer, order 2): a type RESOLVE decides with no claim
//! kind about it, which `enrich extract` refuses, and the retired reader keys;
//! and the fill analysis (order ontology-layer-10): how each declared attribute
//! gets a value, and what nothing fills. Split from `validate.rs` to keep it
//! under arch-gate's 800-line band.

use std::collections::{BTreeMap, BTreeSet};

use super::derived::{FoldBy, DOCUMENT, SUBJECT};
use super::{AttrDecl, AttrFamily, OntologyPolicies, SourceDecl, TypeIndex, TypeKind};
use crate::enrichment::atlas::resolution_records::decides;
use crate::recipe::OntologyBlock;

pub(super) fn layer_warnings(
    block: &OntologyBlock,
    policies: &OntologyPolicies,
    warnings: &mut Vec<String>,
) {
    let orphans = crate::enrichment::atlas::resolution_records::types_without_statements(policies);
    if !orphans.is_empty() {
        warnings.push(format!(
            "RESOLVE decides {} by its identity_criterion, but no claim kind names it as its \
             `subject`: `enrich extract` refuses to build this, since no statement of it would be \
             read. Declare a claim kind about it.",
            orphans
                .iter()
                .map(|t| format!("`{t}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for (key, instead) in crate::recipe_ontology::language::retired_keys(&block.body) {
        warnings.push(format!(
            "[enrichment.ontology] `{key}` is retired and ignored: {instead}. Remove the key."
        ));
    }
    for (t, key, instead) in crate::recipe_ontology::language::retired_type_keys(&block.body) {
        warnings.push(format!(
            "ontology type `{t}`: `{key}` is retired and ignored: {instead}. Remove the key."
        ));
    }
}

/// How one declared attribute gets a value on the default path.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Fill {
    /// The type's `source` reads it from document fields or a table.
    Source,
    /// A declared path or fold derives it (`derived = "<id>"`).
    Derived(String),
    /// The reader's Choose pass asks it, on statements of these claim kinds.
    Choose(Vec<String>),
    /// Nothing does; why, in the declaration's own terms.
    Nothing(Gap),
}

/// Why nothing fills an attribute: the reader pass that would ask it is not
/// built (ONTOLOGY_METHOD §Reading), or it is closed and no Choose reaches it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Gap {
    /// A `ref`: the Pick pass is specced, not built.
    Pick,
    /// Open text, a quantity or a time: the Point pass is specced, not built.
    Point,
    /// Closed, but neither a read claim kind nor the subject of one asks it
    /// (a metadata-sourced subject is asked its identity keys only).
    NotAsked,
}

impl Gap {
    fn why(self) -> &'static str {
        match self {
            Gap::Pick => "a `ref`: the reader's Pick pass is not built",
            Gap::Point => "an open value: the reader's Point pass is not built",
            Gap::NotAsked => "closed, but no read claim kind's Choose asks it",
        }
    }
}

/// The fill analysis: for every attribute every declared type declares, how
/// it is filled, keyed `(type, attribute)` in declaration order. A filler is
/// named from the declaration and the reader's built passes alone
/// (`document_read::reader_chosen_fields`, the plan the reader runs), never
/// from a domain word. `None` when the declaration is read by the general
/// extractor rather than by the passes reader, which this does not judge.
pub(super) fn fill_analysis(
    policies: &OntologyPolicies,
) -> Option<Vec<((String, String), Fill)>> {
    let chosen = crate::enrichment::pipeline::document_read::reader_chosen_fields(policies)?;
    let index = TypeIndex::from_policies(policies);
    let mut out = Vec::new();
    for t in &policies.shape.types {
        for a in &t.attributes {
            let fill = if let Some(id) = &a.derived {
                Fill::Derived(id.clone())
            } else if sourced(t.source.as_ref(), &a.name) {
                Fill::Source
            } else {
                let via: BTreeSet<String> = chosen
                    .iter()
                    .filter(|(_, owner, attr)| *attr == a.name && index.is_a(owner, &t.name))
                    .map(|(claim, _, _)| claim.clone())
                    .collect();
                if via.is_empty() {
                    Fill::Nothing(gap(a))
                } else {
                    Fill::Choose(via.into_iter().collect())
                }
            };
            out.push(((t.name.clone(), a.name.clone()), fill));
        }
    }
    Some(out)
}

fn sourced(source: Option<&SourceDecl>, attr: &str) -> bool {
    match source {
        Some(SourceDecl::Metadata(s)) => {
            s.attributes.contains_key(attr) || s.refs.contains_key(attr)
        }
        Some(SourceDecl::Table(s)) => s.attributes.contains_key(attr),
        None => false,
    }
}

fn gap(a: &AttrDecl) -> Gap {
    match &a.family {
        AttrFamily::Ref { .. } => Gap::Pick,
        AttrFamily::Text { values } if !values.is_empty() => Gap::NotAsked,
        _ => Gap::Point,
    }
}

/// The fill analysis as `recipe validate` output: one note per type naming
/// each attribute's filler (and a claim kind's subject, which RESOLVE or the
/// subject's source decides), one warning per type naming what nothing fills,
/// and one per protocol fold, identity rule or derived step keyed on such an
/// attribute, saying what it then never does.
pub(super) fn fill_warnings(
    policies: &OntologyPolicies,
    warnings: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    let Some(fills) = fill_analysis(policies) else {
        if policies.has_declarations() {
            notes.push(
                "fill: the general extractor reads this declaration (no claim kind the passes \
                 reader reads), so which attributes get a value is not analysed"
                    .to_string(),
            );
        }
        return;
    };
    let index = TypeIndex::from_policies(policies);
    let by_key: BTreeMap<(&str, &str), &Fill> = fills
        .iter()
        .map(|((t, a), f)| ((t.as_str(), a.as_str()), f))
        .collect();
    let unfilled = |t: &str, a: &str| match by_key.get(&(t, a)) {
        Some(Fill::Nothing(g)) => Some(*g),
        _ => None,
    };

    for t in &policies.shape.types {
        let mut parts: Vec<String> = Vec::new();
        if t.kind == TypeKind::Claim {
            if let Some(s) = t.subject.as_deref() {
                parts.push(if decides(&index, s) {
                    format!("subject `{s}` ← RESOLVE")
                } else {
                    format!("subject `{s}`: a sourced type, not RESOLVE's")
                });
            }
        }
        let mut gaps: Vec<String> = Vec::new();
        for a in &t.attributes {
            let Some(fill) = by_key.get(&(t.name.as_str(), a.name.as_str())) else {
                continue;
            };
            parts.push(match fill {
                Fill::Source => format!("{} ← source", a.name),
                Fill::Derived(id) => format!("{} ← derived `{id}`", a.name),
                Fill::Choose(via) => format!("{} ← Choose on {}", a.name, via.join(", ")),
                Fill::Nothing(g) => {
                    gaps.push(format!("`{}` ({})", a.name, g.why()));
                    format!("{} ← nothing", a.name)
                }
            });
        }
        if !parts.is_empty() {
            notes.push(format!("fill: {}: {}", t.name, parts.join("; ")));
        }
        if !gaps.is_empty() {
            warnings.push(format!(
                "ontology type `{}`: nothing fills {}. Each stays empty on every record; give it \
                 a source or a derivation, or drop it.",
                t.name,
                gaps.join(", ")
            ));
        }

        let keys: Vec<(&str, &String)> = [
            ("identity", &t.identity),
            ("identity_fallback", &t.identity_fallback),
            ("identity_necessary", &t.identity_necessary),
        ]
        .into_iter()
        .flat_map(|(rule, ks)| ks.iter().map(move |k| (rule, k)))
        .filter(|(_, k)| unfilled(&t.name, k).is_some())
        .collect();
        if !keys.is_empty() {
            warnings.push(format!(
                "ontology type `{}`: identity is keyed on {}, which nothing fills, so no record \
                 ever has a value there and the key never links or forbids a link.",
                t.name,
                keys.iter()
                    .map(|(rule, k)| format!("`{k}` ({rule})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    let d = &policies.derivation.derived;
    for fold in d.folds.iter().filter(|f| f.by == FoldBy::Protocol) {
        let Some(protocol) = &fold.protocol else {
            continue;
        };
        let target = policies
            .shape
            .types
            .iter()
            .flat_map(|t| t.attributes.iter().map(move |a| (t, a)))
            .filter(|(_, a)| a.derived.as_deref() == Some(fold.id.as_str()))
            .map(|(t, a)| format!("`{}.{}`", t.name, a.name))
            .collect::<Vec<_>>()
            .join(", ");
        let target = if target.is_empty() {
            "its target".to_string()
        } else {
            target
        };
        let mut said: BTreeSet<String> = BTreeSet::new();
        for rule in &protocol.rules {
            let c = rule.claim_kind.as_str();
            let mut say = |field: &str, role: &str, effect: String| {
                if let Some(g) = unfilled(c, field) {
                    let line = format!(
                        "derived fold `{}`: {role} `{c}.{field}`, which nothing fills ({}), so {effect}.",
                        fold.id,
                        g.why()
                    );
                    if said.insert(line.clone()) {
                        warnings.push(line);
                    }
                }
            };
            say(
                &protocol.identity,
                "protocol.identity",
                format!("every transition of `{c}` stays pending and {target} never folds from it"),
            );
            if let Some(f) = &protocol.effective_time {
                say(
                    f,
                    "protocol.effective_time",
                    format!("every transition of `{c}` stays pending and {target} never folds from it"),
                );
            }
            for field in rule.when.keys() {
                say(
                    field,
                    &format!("rule `{}` qualifies on", rule.id),
                    "the rule never matches".to_string(),
                );
            }
            if let Some(f) = &rule.corrects {
                say(
                    f,
                    &format!("rule `{}` corrects by", rule.id),
                    "it never corrects a prior transition".to_string(),
                );
            }
        }
    }

    // A derived path or fold steps through `ref` attributes; one that only
    // unfilled declarations carry reaches nothing.
    let ids = d.paths.iter().map(|x| &x.id).chain(d.folds.iter().map(|x| &x.id));
    for id in ids {
        let steps: BTreeSet<String> = d
            .exprs(id)
            .unwrap_or_default()
            .iter()
            .flat_map(|e| {
                e.steps()
                    .into_iter()
                    .map(|(n, _)| n.to_string())
                    .collect::<Vec<_>>()
            })
            .filter(|n| n != SUBJECT && n != DOCUMENT && d.get(n).is_none())
            .collect();
        for step in steps {
            let declared: Vec<Option<Gap>> = fills
                .iter()
                .filter(|((_, a), _)| *a == step)
                .map(|((t, a), _)| unfilled(t, a))
                .collect();
            if let Some(Some(g)) = declared.first().copied().filter(|_| {
                declared.iter().all(Option::is_some)
            }) {
                warnings.push(format!(
                    "derived `{id}` steps through `{step}`, which nothing fills on any type \
                     declaring it ({}), so it reaches nothing.",
                    g.why()
                ));
            }
        }
    }
}

#[cfg(test)]
#[path = "validate_layer_tests.rs"]
mod tests;
