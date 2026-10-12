// SPDX-License-Identifier: AGPL-3.0-or-later
//! Locate (ONTOLOGY_METHOD §Reading): which declared claim kind each asked
//! line states, and the statements consecutive lines of one kind make. Split
//! from `passes.rs`, which asks it.

use std::ops::Range;

use oicp_types::forced_choice;
use tracing::debug;

use super::passes::{argmax, excerpt, numbered, render, FieldPlan, Line, Plan};
use crate::enrichment::atlas::resolve_records::{decision_wave, LABELS, NONE};
use crate::enrichment::pipeline::types::ChatPrompt;
use crate::InferenceFn;

const LOCATE_SYSTEM: &str = include_str!("passes_locate_prompt.md");
/// What `{values}` in [`LOCATE_SYSTEM`] becomes when some kind in the question
/// shows closed values; nothing otherwise, so the prompt never describes lines
/// it does not show (a declaration with none asks the pre-E4 question).
const LOCATE_SHOWS_VALUES: &str = ", followed by the values its fields can take";

/// A statement is one kind and at most this many verified lines
/// (ONTOLOGY_METHOD §Reading).
pub(super) const MAX_STATEMENT_LINES: usize = 3;

const LOCATE_PHASE: &str = "document_passes_locate";
/// The Locate question for `line`: the whole document with its lines
/// numbered (a prefix every line of the document shares), then the line and
/// the declared kinds, each with its description and, under it, each of its
/// closed-valued fields with the declared values and the field's description.
/// The values are the words a line uses to state the kind: a recipe that put
/// them only in a field's description went unseen here (order 8, abfe32a14:
/// 14 uv states read by a blind recipe that put them in its kind's text).
pub(super) fn locate_question(
    facts: &str,
    body: &str,
    lines: &[Line],
    line: &Line,
    plan: &Plan<'_>,
    labels: &[&str],
) -> ChatPrompt {
    let mut u = format!("{facts}{}", numbered(body, lines));
    u.push_str(&format!(
        "\n\nLine {}: \"{}\"\n\nWhich kind of claim does line {} state?\n",
        line.n,
        &body[line.start..line.end],
        line.n
    ));
    for (kind, label) in plan
        .located()
        .into_iter()
        .map(|k| &plan.kinds[k])
        .zip(labels)
    {
        u.push_str(&format!("{label} {}", kind.decl.name));
        if !kind.decl.description.is_empty() {
            u.push_str(&format!(" ({})", kind.decl.description));
        }
        u.push('\n');
        for field in &kind.fields {
            let FieldPlan::Choose(attr) = field else {
                continue;
            };
            u.push_str(&format!("   {}: {}", attr.name, attr.values.join(", ")));
            if !attr.description.is_empty() {
                u.push_str(&format!(" ({})", attr.description));
            }
            u.push('\n');
        }
    }
    u.push_str(&format!("{NONE} none of them\nAnswer with its letter."));
    ChatPrompt::new(locate_system(plan), u)
        .with_response_schema("read", forced_choice::schema(labels))
        .with_phase_id(LOCATE_PHASE)
        .with_temperature(0.0)
}

/// The Locate system prompt for `plan`: a pure function of the declaration.
pub(super) fn locate_system(plan: &Plan<'_>) -> String {
    let shows_values = plan.located().into_iter().any(|k| {
        plan.kinds[k]
            .fields
            .iter()
            .any(|f| matches!(f, FieldPlan::Choose(_)))
    });
    LOCATE_SYSTEM.replace(
        "{values}",
        if shows_values {
            LOCATE_SHOWS_VALUES
        } else {
            ""
        },
    )
}

/// Runs of consecutive located lines of one kind, each cut into statements
/// of at most `MAX_STATEMENT_LINES`: (kind index, line indices).
pub(super) fn statements(located: &[Option<usize>]) -> Vec<(usize, Range<usize>)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < located.len() {
        let Some(kind) = located[i] else {
            i += 1;
            continue;
        };
        let mut j = i;
        while j < located.len() && located[j] == Some(kind) && j - i < MAX_STATEMENT_LINES {
            j += 1;
        }
        out.push((kind, i..j));
        i = j;
    }
    out
}

/// What Locate read of one document: per line, the kind it states (index in
/// `Plan::kinds`) or none; how many lines it asked, and how many of those
/// questions were refused.
pub(super) struct Located {
    pub(super) kinds: Vec<Option<usize>>,
    pub(super) asked: usize,
    pub(super) refused: usize,
}

/// Locate the lines `asking` (indices into `lines`) of document `id` among
/// the kinds no protocol rule names (`Plan::located`). No line's question
/// needs another's answer, so they go as one wave off the document
/// (`decision_wave`). With every kind an act, nothing is asked.
pub(super) async fn locate(
    (infer, facts, id, raw): (&InferenceFn, &str, &str, &str),
    lines: &[Line],
    asking: &[usize],
    plan: &Plan<'_>,
) -> Located {
    let located_kinds = plan.located();
    let mut out = Located {
        kinds: vec![None; lines.len()],
        asked: 0,
        refused: 0,
    };
    if located_kinds.is_empty() {
        debug!(document = %id, lines = asking.len(), "document_read/passes: every kind is an act; nothing located");
        return out;
    }
    let labels: Vec<&str> = LABELS[..located_kinds.len()]
        .iter()
        .copied()
        .chain([NONE])
        .collect();
    let prompts: Vec<ChatPrompt> = asking
        .iter()
        .map(|&i| locate_question(facts, raw, lines, &lines[i], plan, &labels))
        .collect();
    let items: Vec<String> = asking
        .iter()
        .map(|&i| format!("line {}", lines[i].n))
        .collect();
    out.asked = prompts.len();
    let answers = decision_wave(
        infer,
        &prompts,
        &vec![labels.clone(); prompts.len()],
        id,
        &items,
    )
    .await;
    for (&i, answer) in asking.iter().zip(answers) {
        let line = &lines[i];
        match answer {
            Ok(dist) => {
                let (best, p) = argmax(&labels, &dist);
                let kind = (best < located_kinds.len()).then(|| located_kinds[best]);
                // Every line's answer, located or not, so a missed statement
                // shows how near it came (crm-proof baseline, 2026-10-09).
                debug!(
                    document = %id,
                    line = line.n,
                    kind = kind.map_or("none", |k| plan.kinds[k].decl.name.as_str()),
                    p,
                    dist = %render(&dist),
                    text = %excerpt(&raw[line.start..line.end]),
                    "document_read/passes: located"
                );
                out.kinds[i] = kind;
            }
            Err(refusal) => {
                debug!(document = %id, line = line.n, ?refusal, "document_read/passes: locate refused");
                out.refused += 1;
            }
        }
    }
    out
}
