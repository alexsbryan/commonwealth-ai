// SPDX-License-Identifier: AGPL-3.0-or-later
//! Axis 4 — what holds when: supersession on a declared clock.
//!
//! `governance.rs` owns the oplog and the fold over ACTS. This module owns
//! the one thing a declared ontology adds to that fold: `change.supersedes`
//! names claim types that retire their own earlier instances, and
//! `change.clock` (or a named time attribute) says on what ordering.
//! [`derive_active_with_policy`] is `derive_active` plus that third pass.
//!
//! Split out of `governance.rs` rather than added to it: that file was 915
//! lines and this is a second subject (the log's verdicts vs. the atlas's
//! clock), so the two would have crossed ARCH §3.1's 1200-line limit
//! together while reading as one thing.
//!
//! **The derivation stays OUT of `RuleStatus`.** `ActiveSet` is what the
//! ACTS decided; a clock inference that entered that enum would be
//! indistinguishable from a decision at every reader downstream, and
//! `RuleStatus::Superseded` would have to carry an invented `OpId` for an
//! act nobody performed (ARCH §18.3). So the fold returns the active set
//! UNCHANGED plus a separate [`ClockSupersessions`] map, and the view
//! renders the two side by side.
//!
//! **Linked, and only linked.** The design decision this module exists to
//! enforce is that recency alone never folds a rule. Two rules about one
//! topic at different times are usually both current — that is the Maple
//! House decoy the governance fixture plants — so a pair folds only when
//! something in the corpus SAYS they are the same rule: a reified `same_as`
//! Claim, or the author's own `ref` attribute. No op is written and none is
//! cited; the oplog stays the only writer of adjudications, and this is a
//! query re-derived on every read.

use std::collections::BTreeMap;
use std::path::Path;

use super::governance::{derive_active, ActiveSet, GovernanceOpKind, RuleStatus};
use crate::enrichment::atlas::atoms::{AtomEnvelope, AtomId, AtomsFile, Claim};
use crate::enrichment::ontology::{clock::section_date, AttrFamily, ChangePolicy, OntologyPolicies};
use crate::error::{Error, Result};
use crate::oplog::Op;

/// What [`derive_active_with_policy`] needs to know about one rule atom.
///
/// Deliberately not the atom: the fold is pure over these five fields and
/// says nothing about how they were read off `atoms.json`, which is
/// `governance_view`'s job and changes as the ontology grows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleFacts {
    /// The rule's atom id.
    pub id: AtomId,
    /// The declared claim type (`Claim::claim_kind`). Only types named in
    /// `change.supersedes` fold.
    pub claim_kind: Option<String>,
    /// What the rule is about — `Claim::subject`, falling back to
    /// `attributed_to`. Rules about different subjects never fold into
    /// each other however similar their text.
    pub subject: Option<AtomId>,
    /// The rule's position on its type's clock, already resolved to a
    /// comparable string (an ISO date, or the start of a validity range).
    /// `None` means "no clock", and a rule with no clock neither
    /// supersedes nor is superseded.
    pub clock: Option<String>,
    /// Rules this one has been declared the same as — a reified `same_as`
    /// merge. One of the two ways a pair is LINKED.
    pub same_as: Vec<AtomId>,
    /// The rule this one names as the one it replaces, via a declared
    /// `ref` attribute. The other way a pair is LINKED.
    pub supersedes_ref: Option<AtomId>,
}

/// Rules the clock retired: older rule id → the newer rules that retired
/// it. Deliberately NOT a `RuleStatus` — see the module doc.
pub type ClockSupersessions = BTreeMap<AtomId, Vec<AtomId>>;

/// [`derive_active`] plus a third pass: fold a declared claim type on its
/// declared clock.
///
/// Returns the act-fold's `ActiveSet` unchanged, plus what the CLOCK says.
/// Pass 3 runs per claim type named in [`ChangePolicy::supersedes`], over
/// rules grouped by subject. Within a group, an older rule LINKED to a
/// newer one is retired by the clock. The oplog wins: a rule the log has
/// already spoken about (superseded, retracted) is left off the map
/// entirely, because an act is a decision and this is an inference.
///
/// **Linked, and only linked.** Two rules about the same subject at
/// different times are the Maple House decoy — "guests must leave by 11pm"
/// and "guests must sign in" are both current, and folding the older into
/// the newer because it is older would delete live law. So a pair folds
/// only when something in the corpus SAYS they are the same rule: a
/// reified `same_as` claim (`RuleFacts::same_as`) or the author's own
/// `ref` attribute (`RuleFacts::supersedes_ref`). Recency alone is never
/// enough.
///
/// Writes no op. The oplog stays the only writer of adjudications; this is
/// a query over the atlas, re-derived on every read.
pub fn derive_active_with_policy(
    ops: &[Op<GovernanceOpKind>],
    rules: &[RuleFacts],
    policy: &ChangePolicy,
) -> (ActiveSet, ClockSupersessions) {
    let set = derive_active(ops);
    let mut folded_by_clock = ClockSupersessions::new();
    if policy.supersedes.is_empty() {
        return (set, folded_by_clock);
    }

    let by_id: BTreeMap<&AtomId, &RuleFacts> = rules.iter().map(|r| (&r.id, r)).collect();
    let mut folded = 0usize;

    for kind in policy.supersedes.keys() {
        // Group this type's rules by subject. A rule with no subject
        // groups with nothing — it has not been shown to be about the
        // same thing as anything else.
        let mut groups: BTreeMap<&AtomId, Vec<&RuleFacts>> = BTreeMap::new();
        for r in rules
            .iter()
            .filter(|r| r.claim_kind.as_deref() == Some(kind.as_str()))
        {
            if let Some(subject) = &r.subject {
                groups.entry(subject).or_default().push(r);
            }
        }

        for members in groups.values() {
            for older in members {
                let Some(older_clock) = older.clock.as_deref() else {
                    continue;
                };
                // Every rule in the group this one is LINKED to.
                let linked = members.iter().filter(|newer| {
                    newer.id != older.id && are_linked(older, newer, &by_id)
                });
                let by_rules: Vec<AtomId> = linked
                    .filter(|newer| {
                        newer
                            .clock
                            .as_deref()
                            .is_some_and(|newer_clock| newer_clock > older_clock)
                    })
                    .map(|newer| newer.id.clone())
                    .collect();
                if by_rules.is_empty() {
                    continue;
                }
                // The log outranks the clock: a rule an act already
                // disposed of keeps the act's verdict and is not reported
                // here at all.
                match set.rules.get(&older.id) {
                    Some(RuleStatus::Active) | None => {
                        folded_by_clock.insert(older.id.clone(), by_rules);
                        folded += 1;
                    }
                    Some(_) => {}
                }
            }
        }
    }

    tracing::debug!(
        target: "governance.fold",
        types = ?policy.supersedes.keys().collect::<Vec<_>>(),
        rules = rules.len(),
        folded,
        "derive_active_with_policy: clock supersession"
    );
    (set, folded_by_clock)
}

/// Are `a` and `b` declared to be the same rule? Either direction of a
/// reified `same_as`, or either side naming the other through its
/// declared `ref` attribute.
fn are_linked(a: &RuleFacts, b: &RuleFacts, by_id: &BTreeMap<&AtomId, &RuleFacts>) -> bool {
    if a.same_as.contains(&b.id) || b.same_as.contains(&a.id) {
        return true;
    }
    if a.supersedes_ref.as_ref() == Some(&b.id) || b.supersedes_ref.as_ref() == Some(&a.id) {
        return true;
    }
    // A `same_as` claim is itself an atom, so the link may sit on a third
    // rule naming both. One hop only: two rules that merely share a
    // neighbour are not thereby the same rule.
    by_id
        .values()
        .any(|r| r.same_as.contains(&a.id) && r.same_as.contains(&b.id))
}

/// Project the Claim atoms into the facts axis 4's fold reads.
///
/// The clock is resolved HERE, not in the fold, because only this side
/// knows where a date can come from:
///
/// 1. the declared time attribute `change.supersedes` names for the type
///    (`{ rule = "valid" }` → `attributes["valid"]`), read at its start
///    when it is a range;
/// 2. `attributes["document_date"]`, when something upstream stamped one;
/// 3. the evidence chunk id, read by
///    [`crate::enrichment::ontology::clock::section_date`] — which finds a
///    date only when the section id carries one, and returns `None`
///    otherwise rather than guessing.
///
/// A rule none of the three can date has `clock: None` and neither
/// supersedes nor is superseded (ARCH §18.3 — absence is reported, not
/// defaulted to "now").
pub(crate) fn read_rule_facts(dir: &Path, policies: &OntologyPolicies) -> Result<Vec<RuleFacts>> {
    let change = &policies.change;
    let path = dir.join("atoms.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(&path).map_err(Error::Io)?;
    let file: AtomsFile = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Extraction(format!("governance_view: atoms.json: {e}")))?;

    // The reified merges, indexed by the rules they join, so a `same_as`
    // Claim written by the Phase-6 classifier or the reconciler links its
    // two endpoints.
    let mut same_as: BTreeMap<AtomId, Vec<AtomId>> = BTreeMap::new();
    for a in &file.atoms {
        let AtomEnvelope::Claim(c) = a else { continue };
        if c.claim_kind.as_deref() != Some("same_as") {
            continue;
        }
        let merged: Vec<AtomId> = c
            .attributes
            .get("merged")
            .and_then(|v| v.as_array())
            .map(|ids| {
                ids.iter()
                    .filter_map(|v| v.as_str())
                    .map(AtomId::from_raw)
                    .collect()
            })
            .unwrap_or_default();
        for id in &merged {
            let mut others: Vec<AtomId> =
                merged.iter().filter(|o| *o != id).cloned().collect();
            same_as.entry(id.clone()).or_default().append(&mut others);
        }
    }

    Ok(file
        .atoms
        .iter()
        .filter_map(|a| match a {
            AtomEnvelope::Claim(c) => Some(c),
            _ => None,
        })
        .map(|c| RuleFacts {
            id: c.id.clone(),
            claim_kind: c.claim_kind.clone(),
            subject: c.subject.clone().or_else(|| c.attributed_to.clone()),
            clock: rule_clock(c, change),
            same_as: same_as.get(&c.id).cloned().unwrap_or_default(),
            supersedes_ref: c
                .claim_kind
                .as_deref()
                .and_then(|k| ref_to_own_type(policies, k))
                .and_then(|attr| c.attributes.get(attr))
                .and_then(|v| v.as_str())
                .map(AtomId::from_raw),
        })
        .collect())
}

/// The declared attribute by which an instance of `kind` names another
/// instance of `kind` — a rule pointing at the rule it replaces.
///
/// DERIVED from the declaration rather than a fixed key: an author writes
/// `{ name = "replaces", type = "ref", of = "rule" }` or
/// `{ name = "amends", ... }` and either must work. A hard-coded
/// `"supersedes"` would silently do nothing for every author who chose a
/// different noun (ARCH §2.1). `None` when the type declares no such
/// attribute, which is every shipped template today — the `same_as` link
/// is the path that carries weight.
fn ref_to_own_type<'a>(policies: &'a OntologyPolicies, kind: &str) -> Option<&'a str> {
    policies.type_decl(kind)?.attributes.iter().find_map(|a| {
        matches!(&a.family, AttrFamily::Ref { of } if of == kind).then_some(a.name.as_str())
    })
}

/// Where one rule sits on its clock — see [`read_rule_facts`] for the
/// three sources, in order.
fn rule_clock(c: &Claim, change: &ChangePolicy) -> Option<String> {
    let declared = c
        .claim_kind
        .as_deref()
        .and_then(|k| change.supersedes.get(k))
        .filter(|attr| attr.as_str() != "document_date");
    if let Some(attr) = declared {
        if let Some(v) = c.attributes.get(attr).and_then(|v| v.as_str()) {
            // A validity RANGE speaks from its start.
            let start = v.split('/').next().unwrap_or(v).trim();
            if !start.is_empty() {
                return Some(start.to_string());
            }
        }
    }
    if let Some(v) = c.attributes.get("document_date").and_then(|v| v.as_str()) {
        if !v.trim().is_empty() {
            return Some(v.trim().to_string());
        }
    }
    c.evidence
        .first()
        .and_then(|e| section_date(&e.chunk_id))
        .map(|d| d.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(n: usize) -> AtomId {
        AtomId::claim(n)
    }

    /// Build an op at a monotonic, explicit timestamp so ids are
    /// deterministic and distinct within a test.
    fn op(kind: GovernanceOpKind, ts: i64, actor: &str) -> Op<GovernanceOpKind> {
        Op::new(kind, ts, actor)
    }

    fn facts(n: usize, subject: usize, clock: &str) -> RuleFacts {
        RuleFacts {
            id: rule(n),
            claim_kind: Some("rule".into()),
            subject: Some(AtomId::entity(subject)),
            clock: Some(clock.into()),
            same_as: Vec::new(),
            supersedes_ref: None,
        }
    }

    fn rule_clock_policy() -> ChangePolicy {
        let mut p = ChangePolicy::default();
        p.supersedes.insert("rule".into(), "valid".into());
        p
    }

    #[test]
    fn derive_active_with_policy_folds_linked_only() {
        // Three rules on one topic. r1 and r2 are declared the same rule
        // and r2 is later, so r1 folds. r3 is later still but LINKED to
        // nothing — the Maple House decoy — and must stay in force.
        let mut r1 = facts(1, 1, "2024-01-01");
        let r2 = facts(2, 1, "2025-03-14");
        let r3 = facts(3, 1, "2025-09-01");
        r1.same_as = vec![rule(2)];

        let ops = vec![
            op(
                GovernanceOpKind::AssertRule {
                    rule: rule(1),
                    source_doc: None,
                },
                1000,
                "ingest",
            ),
            op(
                GovernanceOpKind::AssertRule {
                    rule: rule(2),
                    source_doc: None,
                },
                1001,
                "ingest",
            ),
            op(
                GovernanceOpKind::AssertRule {
                    rule: rule(3),
                    source_doc: None,
                },
                1002,
                "ingest",
            ),
        ];

        let (set, by_clock) = derive_active_with_policy(&ops, &[r1, r2, r3], &rule_clock_policy());
        assert_eq!(
            by_clock.get(&rule(1)),
            Some(&vec![rule(2)]),
            "the linked older rule folds"
        );
        assert!(
            !by_clock.contains_key(&rule(3)),
            "an unlinked later rule is not a supersession — it is more law"
        );
        // The ACT fold is untouched: every rule the log asserted is still
        // Active there, because the clock decided nothing.
        for n in 1..=3 {
            assert_eq!(set.rules.get(&rule(n)), Some(&RuleStatus::Active));
        }
    }

    #[test]
    fn derive_active_with_policy_never_folds_across_subjects() {
        let mut r1 = facts(1, 1, "2024-01-01");
        // Same link, DIFFERENT subject: two rules that are somehow
        // declared the same but govern different topics do not fold,
        // because the grouping is by subject first.
        let r2 = facts(2, 2, "2025-03-14");
        r1.same_as = vec![rule(2)];
        let ops = vec![
            op(
                GovernanceOpKind::AssertRule {
                    rule: rule(1),
                    source_doc: None,
                },
                1000,
                "ingest",
            ),
            op(
                GovernanceOpKind::AssertRule {
                    rule: rule(2),
                    source_doc: None,
                },
                1001,
                "ingest",
            ),
        ];
        let (_set, by_clock) = derive_active_with_policy(&ops, &[r1, r2], &rule_clock_policy());
        assert!(by_clock.is_empty(), "grouping is by subject first");
    }

    #[test]
    fn derive_active_with_policy_leaves_undeclared_corpora_alone() {
        // No `change.supersedes` — the fold is exactly `derive_active`,
        // whatever facts it is handed.
        let mut r1 = facts(1, 1, "2024-01-01");
        r1.same_as = vec![rule(2)];
        let r2 = facts(2, 1, "2025-03-14");
        let ops = vec![op(
            GovernanceOpKind::AssertRule {
                rule: rule(1),
                source_doc: None,
            },
            1000,
            "ingest",
        )];
        let (set, by_clock) = derive_active_with_policy(&ops, &[r1, r2], &ChangePolicy::default());
        assert_eq!(set, derive_active(&ops));
        assert!(by_clock.is_empty());
    }

    #[test]
    fn derive_active_with_policy_lets_the_log_outrank_the_clock() {
        // r1 was RETRACTED by an act. The clock would call it superseded;
        // the act wins, because an act is a decision and this is not.
        let mut r1 = facts(1, 1, "2024-01-01");
        r1.same_as = vec![rule(2)];
        let r2 = facts(2, 1, "2025-03-14");
        let ops = vec![
            op(
                GovernanceOpKind::AssertRule {
                    rule: rule(1),
                    source_doc: None,
                },
                1000,
                "ingest",
            ),
            op(
                GovernanceOpKind::RetractRule {
                    rule: rule(1),
                    rationale: "withdrawn".into(),
                },
                1001,
                "human:alex",
            ),
        ];
        let (set, by_clock) = derive_active_with_policy(&ops, &[r1, r2], &rule_clock_policy());
        assert!(matches!(
            set.rules.get(&rule(1)),
            Some(RuleStatus::Retracted { .. })
        ));
        assert!(
            !by_clock.contains_key(&rule(1)),
            "a rule an act disposed of is not re-reported by the clock"
        );
    }

    #[test]
    fn derive_active_with_policy_needs_a_clock_on_both_sides() {
        let mut r1 = facts(1, 1, "2024-01-01");
        r1.same_as = vec![rule(2)];
        let mut r2 = facts(2, 1, "2025-03-14");
        r2.clock = None;
        let ops = vec![op(
            GovernanceOpKind::AssertRule {
                rule: rule(1),
                source_doc: None,
            },
            1000,
            "ingest",
        )];
        let (_set, by_clock) = derive_active_with_policy(&ops, &[r1, r2], &rule_clock_policy());
        assert!(
            by_clock.is_empty(),
            "a rule with no clock cannot be shown to be later"
        );
    }
}
