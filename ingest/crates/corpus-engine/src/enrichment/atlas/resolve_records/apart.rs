// SPDX-License-Identifier: AGPL-3.0-or-later
//! What keeps a statement from a record, and the code's own joins that obey
//! it: a declared necessary value that differs, a particular the window's
//! reader read as another (`chapters_compatible`, `read_apart`), a sufficient
//! key's join (`key_plan`), a field's (`settle_field`), and the recheck of every
//! join before fold (`gate_plans`). The model is asked in `select.rs`; what it
//! may be offered and what code may join is decided here.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use tracing::debug;

use super::{Criterion, Plan, Record, Refusal, Statement};

/// Two value sets are compatible when every necessary attribute evidenced on
/// both sides has exactly one shared value. Missing evidence is permissive;
/// contaminated multi-value sets match neither of their members. Called on
/// SUPPLIED values only (a declared field's, never a model's read): those
/// forbid outright. A read value is a weighed source (`compare_necessary`).
pub(super) fn necessary_compatible(
    criterion: &Criterion,
    left: &BTreeMap<String, BTreeSet<String>>,
    right: &BTreeMap<String, BTreeSet<String>>,
) -> bool {
    values_compatible(criterion, left, right)
        && chapters_compatible(
            left.get(super::CHAPTER_SUBJECT_KEY),
            right.get(super::CHAPTER_SUBJECT_KEY),
        )
}

/// What forbids a record a question offers or a forced choice joins: a
/// differing declared necessary value, or the record being wholly another
/// particular of the asked statement's own window (`read_apart`). A record
/// that holds another reference of that window AND another window's is
/// offered, marked (`question`), and the model decides: the window reader's
/// distinction meets another window's reading there, and as a veto it made a
/// reader's in-window split permanent (world-s1 coyoteville-d1, 2026-10-14).
/// Code's own joins, by key or field, keep `necessary_compatible`.
pub(super) fn offerable(
    criterion: &Criterion,
    left: &BTreeMap<String, BTreeSet<String>>,
    right: &BTreeMap<String, BTreeSet<String>>,
) -> bool {
    values_compatible(criterion, left, right)
        && !read_apart(
            left.get(super::CHAPTER_SUBJECT_KEY),
            right.get(super::CHAPTER_SUBJECT_KEY),
        )
}

/// The record is wholly other particulars of the statement's own window, as
/// that window's one reading gave them: a distinction made first-hand, with
/// the whole window in view, which no later question overturns. Measured
/// 2026-10-14 (arm chapter-shown): asked to override it, RESOLVE joined
/// rightly 4 times and wrongly 5 (Ward's three TEP master agreements folded
/// into one), against 31 to 4 where the record also held another window's.
pub(super) fn read_apart(
    left: Option<&BTreeSet<String>>,
    right: Option<&BTreeSet<String>>,
) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    let chapter = |s: &str| s.split_once('/').map_or(s, |(c, _)| c).to_string();
    let mine: BTreeSet<String> = left.iter().map(|a| chapter(a)).collect();
    !chapters_compatible(Some(left), Some(right))
        && right.iter().all(|b| mine.contains(&chapter(b)))
}

/// The window reference a record holds that this document's reader read
/// beside the statement's own: another particular of the statement's window
/// that a statement of this same document (`in_document`) is about. The reader
/// saw both in one document and called them two, so no question joins them;
/// whether particulars of DIFFERENT documents are one is RESOLVE's question
/// (`offerable` marks those). `None` when the record already holds the
/// statement's own reference: its particular is there.
pub(super) fn read_beside<'a>(
    mine: Option<&BTreeSet<String>>,
    held: &'a BTreeSet<String>,
    in_document: &BTreeSet<String>,
) -> Option<&'a String> {
    let mine = mine?;
    if mine.is_subset(held) {
        return None;
    }
    held.iter().find(|h| {
        !mine.contains(*h)
            && in_document.contains(*h)
            && mine.iter().any(|m| chapter_of(m) == chapter_of(h))
    })
}

/// The window a reference was read in: `sec_00002` of `sec_00002/p5`.
fn chapter_of(reference: &str) -> &str {
    reference.split_once('/').map_or(reference, |(c, _)| c)
}

/// The declared necessary values alone.
pub(super) fn values_compatible(
    criterion: &Criterion,
    left: &BTreeMap<String, BTreeSet<String>>,
    right: &BTreeMap<String, BTreeSet<String>>,
) -> bool {
    criterion.necessary.iter().all(|attr| {
        let Some(left) = left.get(&attr.name).filter(|values| !values.is_empty()) else {
            return true;
        };
        let Some(right) = right.get(&attr.name).filter(|values| !values.is_empty()) else {
            return true;
        };
        left.len() == 1 && right.len() == 1 && left.first() == right.first()
    })
}

/// Two references one window's reader gave are two particulars as it read
/// them: a statement never joins a record holding another reference of its
/// chapter. References of other chapters say nothing here.
pub(super) fn chapters_compatible(
    left: Option<&BTreeSet<String>>,
    right: Option<&BTreeSet<String>>,
) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return true;
    };
    let chapter = |s: &str| {
        s.split_once('/')
            .map_or(s.to_string(), |(c, _)| c.to_string())
    };
    left.iter()
        .all(|a| right.iter().all(|b| a == b || chapter(a) != chapter(b)))
}

/// Prior records matching a statement's declared sufficient keys.
pub(super) fn key_hits(
    by_key: &HashMap<(String, String), usize>,
    keys: &[Vec<(String, String)>],
) -> Vec<Vec<(usize, String, String)>> {
    keys.iter()
        .map(|keys| {
            keys.iter()
                .filter_map(|(key, value)| {
                    by_key
                        .get(&(key.clone(), value.clone()))
                        .map(|&record| (record, key.clone(), value.clone()))
                })
                .collect()
        })
        .collect()
}

/// A sufficient key settles only when its record's supplied necessary values agree.
pub(super) fn key_plan(
    criterion: &Criterion,
    statement: &Statement,
    hits: &[(usize, String, String)],
    supplied: &BTreeMap<String, BTreeSet<String>>,
    records: &[Record],
    document: &str,
    vetoed: &mut u32,
) -> Option<Plan> {
    let held: BTreeSet<usize> = hits.iter().map(|(record, _, _)| *record).collect();
    match (held.len(), hits.first()) {
        (0, _) | (_, None) => None,
        (1, Some((record, key, value))) => {
            if necessary_compatible(criterion, supplied, &records[*record].supplied) {
                Some(Plan::Key {
                    record: *record,
                    key: key.clone(),
                    value: value.clone(),
                })
            } else {
                *vetoed += 1;
                debug!(
                    document,
                    statement = %statement.id,
                    candidate = %records[*record].id,
                    route = "sufficient_key",
                    "atlas/resolve: necessary-field conflict vetoed the key join"
                );
                Some(Plan::Refuse(Refusal::Contradiction {
                    targets: vec![records[*record].id.clone()],
                }))
            }
        }
        _ => Some(Plan::Refuse(Refusal::Contradiction {
            targets: held
                .iter()
                .map(|&record| records[record].id.clone())
                .collect(),
        })),
    }
}

/// Apply the fields' plan (`fields::settle`) only where supplied necessary values are compatible.
pub(super) fn settle_field(
    criterion: &Criterion,
    statements: &[Statement],
    supplied: &[BTreeMap<String, BTreeSet<String>>],
    records: &[Record],
    plans: &mut [Option<Plan>],
    field: Option<Plan>,
    document: &str,
    vetoed: &mut u32,
) {
    let Some(Plan::Weighed { record, .. }) = field.as_ref() else {
        return;
    };
    let record = *record;
    for (i, plan) in plans.iter_mut().enumerate() {
        if plan.is_some() {
            continue;
        }
        if necessary_compatible(criterion, &supplied[i], &records[record].supplied) {
            *plan = field.clone();
        } else {
            *vetoed += 1;
            debug!(
                document,
                statement = %statements[i].id,
                candidate = %records[record].id,
                route = "evidential_field",
                "atlas/resolve: necessary-field conflict vetoed the field join"
            );
        }
    }
}

/// Recheck every answerer join and every same-document opened group against
/// necessary values before fold: supplied ones when the answerer is a forced
/// choice, whose decider weighed every read value already; supplied and READ
/// ones (`values`, and a record's `fields`) under a partition answerer, which
/// weighs no source, so a read value can only be honoured as a constraint.
pub(super) fn gate_plans(
    criterion: &Criterion,
    statements: &[Statement],
    asked: &[usize],
    supplied: &[BTreeMap<String, BTreeSet<String>>],
    records: &[Record],
    reads_weighed: bool,
    decided: Vec<Plan>,
    document: &str,
    vetoed: &mut u32,
) -> Vec<Plan> {
    let mut open_groups: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut gated = Vec::with_capacity(decided.len());
    for (j, mut plan) in decided.into_iter().enumerate() {
        let i = asked[j];
        let target = match &plan {
            Plan::Key { record, .. }
            | Plan::Join { record, .. }
            | Plan::Weighed { record, .. }
            | Plan::Chosen { record, .. } => Some(*record),
            Plan::Open { .. } | Plan::Held(_) | Plan::Refuse(_) => None,
        };
        if let Some(record) = target {
            let held = if reads_weighed {
                &records[record].supplied
            } else {
                &records[record].fields
            };
            // A forced choice's question showed the reader's other reference
            // (`question`), so only declared values gate it; a partition
            // answerer is shown no such mark and keeps the window's references.
            let compatible = if reads_weighed {
                offerable(criterion, &supplied[i], held)
            } else {
                necessary_compatible(criterion, &supplied[i], held)
            };
            if !compatible {
                *vetoed += 1;
                debug!(
                    document,
                    statement = %statements[i].id,
                    candidate = %records[record].id,
                    "atlas/resolve: necessary-field conflict vetoed the answerer join"
                );
                plan = Plan::Refuse(Refusal::Contradiction {
                    targets: vec![records[record].id.clone()],
                });
            }
        }
        let open_group = match &plan {
            Plan::Open { group, .. } => Some(*group),
            _ => None,
        };
        if let Some(group) = open_group {
            let conflict = open_groups.get(&group).and_then(|prior| {
                prior
                    .iter()
                    .copied()
                    .find(|&prior| !necessary_compatible(criterion, &supplied[i], &supplied[prior]))
            });
            if let Some(prior) = conflict {
                *vetoed += 1;
                debug!(
                    document,
                    statement = %statements[i].id,
                    candidate = %statements[prior].id,
                    "atlas/resolve: necessary-field conflict vetoed the within-document join"
                );
                plan = Plan::Refuse(Refusal::Contradiction {
                    targets: vec![statements[prior].id.clone()],
                });
            } else {
                open_groups.entry(group).or_default().push(i);
            }
        }
        gated.push(plan);
    }
    gated
}
