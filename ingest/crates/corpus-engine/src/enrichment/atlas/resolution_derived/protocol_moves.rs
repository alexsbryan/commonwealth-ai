// SPDX-License-Identifier: AGPL-3.0-or-later
//! A protocol's transitions (ONTOLOGY_PRIMITIVES §8): a rule is a move into
//! its state, made only by whom its `by` path reaches from the claim and only
//! from a state its `from` lists. Reports are replayed in report-time order
//! from the protocol's `initial` state; a report no move admits stays in the
//! audit history as evidence, never as the record's state. A protocol that
//! declares neither `by` nor `from` is folded as before, by its latest report.

use std::collections::BTreeSet;

use tracing::debug;

use super::{Effects, Graph, Node, Reached};
use crate::enrichment::ontology::derived::{ProtocolFoldDecl, ProtocolRuleDecl};

/// Whether `protocol` is a state machine: some rule names who may move or
/// from which states.
pub(super) fn is_machine(protocol: &ProtocolFoldDecl) -> bool {
    protocol
        .rules
        .iter()
        .any(|r| r.by.is_some() || !r.from.is_empty())
}

/// `matching` split into the rules whose mover the claim satisfies (no `by`,
/// or its path walked from the claim reaches something) and those it does not.
pub(super) fn authorized<'r>(
    graph: &Graph<'_>,
    matching: Vec<&'r ProtocolRuleDecl>,
    claim: usize,
) -> Result<(Vec<&'r ProtocolRuleDecl>, Vec<&'r ProtocolRuleDecl>), String> {
    let (mut may, mut may_not) = (Vec::new(), Vec::new());
    for rule in matching {
        let Some(by) = rule.by.as_deref() else {
            may.push(rule);
            continue;
        };
        let exprs = graph.parsed.get(by).ok_or_else(|| {
            format!(
                "protocol rule `{}`: `by` path `{by}` is not parsed",
                rule.id
            )
        })?;
        let start: Reached = [(
            Node::Claim(claim),
            graph.claim_doc[claim].map(str::to_string),
        )]
        .into();
        let mut fx = Effects::default();
        let mut reached = 0;
        for expr in exprs {
            reached += graph.walk(expr, start.clone(), &mut fx)?.len();
        }
        debug!(claim = %graph.claims[claim].id.as_str(), rule = %rule.id, by, reached, unjudged = fx.unjudged, "atlas/derive: protocol mover tested");
        if reached > 0 {
            may.push(rule);
        } else {
            may_not.push(rule);
        }
    }
    Ok((may, may_not))
}

/// One move a claim may make, for the replay: when it was reported, the
/// state it enters, and the states it may leave (`None`: any, and none).
pub(super) struct Move<'a> {
    pub(super) at: &'a str,
    pub(super) state: &'a str,
    pub(super) from: Option<BTreeSet<&'a str>>,
}

/// The states one claim's rules may leave: `None` when any rule leaves any.
pub(super) fn leaves<'r>(rules: &[&'r ProtocolRuleDecl]) -> Option<BTreeSet<&'r str>> {
    let mut out = BTreeSet::new();
    for rule in rules {
        if rule.from.is_empty() {
            return None;
        }
        out.extend(rule.from.iter().map(String::as_str));
    }
    Some(out)
}

/// What the replay made of the moves, each by its place in the input.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Replay {
    /// The report time of the last move made and the state it entered.
    pub(super) state: Option<(String, String)>,
    /// `selected` (the last moves made), `superseded_by_report_time` (made,
    /// then moved on from), `not_from_state` (no state it may leave was held),
    /// `conflict`, or `qualified` (after a conflict, never reached).
    pub(super) dispositions: Vec<&'static str>,
    /// Moves into different states at one report time: the replay stops there.
    pub(super) conflict: Option<(String, BTreeSet<String>)>,
}

/// Replay `moves` in report-time order from `initial`.
pub(super) fn replay(moves: &[Move<'_>], initial: Option<&str>) -> Replay {
    let mut order: Vec<usize> = (0..moves.len()).collect();
    order.sort_by_key(|&i| moves[i].at);
    let mut out = Replay {
        dispositions: vec!["qualified"; moves.len()],
        ..Replay::default()
    };
    let mut held: Option<&str> = initial;
    let mut last: Vec<usize> = Vec::new();
    let mut k = 0;
    while k < order.len() {
        let at = moves[order[k]].at;
        let group: Vec<usize> = order[k..]
            .iter()
            .copied()
            .take_while(|&i| moves[i].at == at)
            .collect();
        k += group.len();
        let admits = |m: &Move<'_>| {
            m.from
                .as_ref()
                .is_none_or(|f| held.is_some_and(|h| f.contains(h)))
        };
        let (made, refused): (Vec<usize>, Vec<usize>) =
            group.iter().partition(|&&i| admits(&moves[i]));
        for &i in &refused {
            out.dispositions[i] = "not_from_state";
            debug!(at, state = moves[i].state, held = ?held, "atlas/derive: protocol report admits no move from the held state; kept as evidence");
        }
        let entered: BTreeSet<&str> = made.iter().map(|&i| moves[i].state).collect();
        match entered.len() {
            0 => {}
            1 => {
                for &i in &last {
                    out.dispositions[i] = "superseded_by_report_time";
                }
                for &i in &made {
                    out.dispositions[i] = "selected";
                }
                let state = *entered.first().unwrap();
                debug!(at, from = ?held, to = state, "atlas/derive: protocol move made");
                held = Some(state);
                out.state = Some((at.to_string(), state.to_string()));
                last = made;
            }
            _ => {
                for &i in &made {
                    out.dispositions[i] = "conflict";
                }
                debug!(at, states = ?entered, "atlas/derive: protocol moves into different states at one report time");
                out.conflict = Some((
                    at.to_string(),
                    entered.into_iter().map(str::to_string).collect(),
                ));
                return out;
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "protocol_moves_tests.rs"]
mod tests;
