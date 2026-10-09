// SPDX-License-Identifier: AGPL-3.0-or-later
//! READ by pointing among declared values (ONTOLOGY_METHOD.md §Identity,
//! Ring 1b of `research/ontology-apps/resolve-prereg.md`): for each of a
//! type's `identity_necessary` attributes, one forced choice per statement
//! over the attribute's declared `values`, read as a distribution in one
//! forward pass through RESOLVE's census funnel. Asked whole, the model
//! resolves one level coarser than the criterion (GVC: the incident, not the
//! kind of happening); asked the attribute alone, it reads it (.721 on GVC
//! dev), and code compares.

use std::collections::{BTreeMap, BTreeSet};

use oicp_types::forced_choice;
use tracing::{debug, warn};

use super::select::{decision_call, LABELS, NONE};
use super::{marked_context, ClosedAttr, Criterion, Document, Statement};
use crate::enrichment::pipeline::types::ChatPrompt;
use crate::InferenceFn;

const SYSTEM: &str = include_str!("../resolve_read_prompt.md");

/// Necessary values supplied or READ for each asked statement (by position in
/// `asked`), the calls made, and the reads that came back unknown or refused.
pub(super) struct Read {
    pub values: Vec<BTreeMap<String, BTreeSet<String>>>,
    pub calls: u32,
    pub unknown: u32,
}

/// Necessary values explicitly supplied with a statement are evidence only
/// when they belong to the attribute's declared closed set.
pub(super) fn provided(
    criterion: &Criterion,
    doc: Document<'_>,
    statements: &[Statement],
) -> Vec<BTreeMap<String, BTreeSet<String>>> {
    let mut out: Vec<BTreeMap<String, BTreeSet<String>>> = vec![BTreeMap::new(); statements.len()];
    for (i, statement) in statements.iter().enumerate() {
        for attr in &criterion.necessary {
            let Some(value) = statement.keys.get(&attr.name) else {
                continue;
            };
            if attr.values.contains(value) {
                out[i]
                    .entry(attr.name.clone())
                    .or_default()
                    .insert(value.clone());
            } else {
                warn!(
                    document = doc.id,
                    statement = %statement.id,
                    attr = %attr.name,
                    value,
                    allowed = ?attr.values,
                    "atlas/resolve read: supplied necessary value is outside the declared set"
                );
            }
        }
    }
    out
}

/// READ every necessary attribute of every asked statement. A value is kept
/// only when its label is the most probable; a validated supplied value skips
/// the call, while "none of them" and a refusal leave the value unknown.
pub(super) async fn read(
    criterion: &Criterion,
    doc: Document<'_>,
    statements: &[Statement],
    asked: &[usize],
    provided: &[BTreeMap<String, BTreeSet<String>>],
    infer: &InferenceFn,
) -> Read {
    let mut out = Read {
        values: vec![BTreeMap::new(); asked.len()],
        calls: 0,
        unknown: 0,
    };
    for (j, &i) in asked.iter().enumerate() {
        out.values[j] = provided[i].clone();
        let s = &statements[i];
        for necessary in &criterion.necessary {
            let (attr, values) = (necessary.name.as_str(), &necessary.values);
            if out.values[j].contains_key(attr) {
                continue;
            }
            let labels: Vec<&str> = LABELS[..values.len()]
                .iter()
                .copied()
                .chain([NONE])
                .collect();
            let prompt = choice_question(
                &criterion.type_name,
                &criterion.description,
                necessary,
                &labels,
                doc.body,
                s.start..s.end,
                "resolve_read",
            );
            out.calls += 1;
            let dist = match decision_call(infer, &prompt, &labels, doc.id, &s.id).await {
                Ok(dist) => dist,
                Err(refusal) => {
                    debug!(document = doc.id, statement = %s.id, attr, ?refusal, "atlas/resolve read: refused, the attribute stays unknown");
                    out.unknown += 1;
                    continue;
                }
            };
            let best = labels
                .iter()
                .copied()
                .fold((NONE, f64::NEG_INFINITY), |a, l| {
                    if dist[l] > a.1 {
                        (l, dist[l])
                    } else {
                        a
                    }
                });
            match labels.iter().position(|&l| l == best.0) {
                Some(k) if best.0 != NONE => {
                    debug!(document = doc.id, statement = %s.id, attr, value = %values[k], p = best.1, "atlas/resolve read");
                    out.values[j]
                        .entry(attr.to_string())
                        .or_default()
                        .insert(values[k].clone());
                }
                _ => {
                    debug!(document = doc.id, statement = %s.id, attr, p = best.1, "atlas/resolve read: none of the values");
                    out.unknown += 1;
                }
            }
        }
    }
    out
}

/// The one-attribute question: the type and the attribute, each with its
/// declared description, the attribute's values, and the statement at `span`
/// of `body` marked in its passage. RESOLVE asks it as `resolve_read`; the
/// passes reader asks the same question of claim fields under its own phase.
pub(crate) fn choice_question(
    type_name: &str,
    type_description: &str,
    attr: &ClosedAttr,
    labels: &[&str],
    body: &str,
    span: std::ops::Range<usize>,
    phase: &str,
) -> ChatPrompt {
    let mut u = format!("Type: {type_name}");
    if !type_description.is_empty() {
        u.push_str(&format!(" ({type_description})"));
    }
    u.push_str(&format!("\nAttribute: {}", attr.name));
    if !attr.description.is_empty() {
        u.push_str(&format!(" ({})", attr.description));
    }
    u.push_str(&format!(
        "\n\nStatement, its words in [[ ]]: \"…{}…\"\n\nWhich {} do the words in [[ ]] refer to?\n",
        marked_context(body, span.start, span.end),
        attr.name
    ));
    for (v, l) in attr.values.iter().zip(labels) {
        u.push_str(&format!("{l} {v}\n"));
    }
    u.push_str(&format!(
        "{NONE} none of them, or the passage does not say\nAnswer with its letter."
    ));
    ChatPrompt::new(SYSTEM, u)
        .with_response_schema("read", forced_choice::schema(labels))
        .with_phase_id(phase)
        .with_temperature(0.0)
}
