// SPDX-License-Identifier: AGPL-3.0-or-later
//! READ by pointing among declared values (ONTOLOGY_METHOD.md §Identity,
//! Ring 1b of `research/ontology-apps/resolve-prereg.md`): for each of a
//! type's `identity_necessary` attributes, one forced choice per statement
//! over the attribute's declared `values`, read as a distribution in one
//! forward pass through RESOLVE's census funnel. Asked whole, the model
//! resolves one level coarser than the criterion (GVC: the incident, not the
//! kind of happening); asked the attribute alone, it reads it (.721 on GVC
//! dev), and code compares.

use std::collections::BTreeMap;

use oicp_types::forced_choice;
use tracing::debug;

use super::select::{decision_call, LABELS, NONE};
use super::{marked_context, Criterion, Document, Statement};
use crate::enrichment::pipeline::types::ChatPrompt;
use crate::InferenceFn;

const SYSTEM: &str = include_str!("../resolve_read_prompt.md");

/// The values READ for each asked statement (by position in `asked`), the
/// calls made, and the reads that came back unknown or refused.
pub(super) struct Read {
    pub values: Vec<BTreeMap<String, String>>,
    pub calls: u32,
    pub unknown: u32,
}

/// READ every necessary attribute of every asked statement. A value is kept
/// only when its label is the most probable; "none of them" and a refused
/// call leave the attribute unknown, counted, never defaulted to a value.
pub(super) async fn read(
    criterion: &Criterion,
    doc: Document<'_>,
    statements: &[Statement],
    asked: &[usize],
    infer: &InferenceFn,
) -> Read {
    let mut out = Read {
        values: vec![BTreeMap::new(); asked.len()],
        calls: 0,
        unknown: 0,
    };
    for (j, &i) in asked.iter().enumerate() {
        let s = &statements[i];
        for (attr, values) in &criterion.necessary {
            let labels: Vec<&str> = LABELS[..values.len()]
                .iter()
                .copied()
                .chain([NONE])
                .collect();
            let prompt = question(criterion, attr, values, &labels, doc, s);
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
                    out.values[j].insert(attr.clone(), values[k].clone());
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

/// The one-attribute question: the type, the attribute and its values, and
/// the statement marked in its passage.
fn question(
    criterion: &Criterion,
    attr: &str,
    values: &[String],
    labels: &[&str],
    doc: Document<'_>,
    s: &Statement,
) -> ChatPrompt {
    let mut u = format!("Type: {}", criterion.type_name);
    if !criterion.description.is_empty() {
        u.push_str(&format!(" ({})", criterion.description));
    }
    u.push_str(&format!(
        "\nAttribute: {attr}\n\nStatement, its words in [[ ]]: \"…{}…\"\n\nWhich {attr} do the words in [[ ]] refer to?\n",
        marked_context(doc.body, s.start, s.end)
    ));
    for (v, l) in values.iter().zip(labels) {
        u.push_str(&format!("{l} {v}\n"));
    }
    u.push_str(&format!(
        "{NONE} none of them, or the passage does not say\nAnswer with its letter."
    ));
    ChatPrompt::new(SYSTEM, u)
        .with_response_schema("read", forced_choice::schema(labels))
        .with_phase_id("resolve_read")
        .with_temperature(0.0)
}
