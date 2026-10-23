// SPDX-License-Identifier: AGPL-3.0-or-later
//! The statement kinds the method supplies for a record whose state a
//! protocol fold derives (ONTOLOGY_METHOD §Lifecycle): one that the record is
//! replaced, rolled into or superseded by another, and one for something done
//! under it as it stands. The reader reports which it read; the fold applies
//! what it means, so no prompt rule stands in for either.
//!
//! A recipe opts in: `lifecycle = true`, or a pinned lever. Supplied by
//! default they failed their bars (`research/ontology-apps/lifecycle-prereg.md`
//! Result: the reader read a replaced deal's end as won 2 times in 4, and on
//! real mail wrote stage moves as acts under), so the default is today's
//! reader. Opted in, what each means is the recipe's to say, with the sources'
//! meaning unless it pins another: a replaced record ends, its state given by
//! the recipe's own rules from their values (the replaced kind is a kind of the
//! fold's claim kind), or `replaced = "<state>"`; an act under a record (a
//! nomination or a draw under a trade, CDM's reset) changes nothing, or
//! `acts_under = "<state>"` makes it a move.

use std::collections::BTreeSet;

use tracing::{debug, info};
use understanding_vocab::ontology::decl::{AttrFamily, Force, OntologyTypeDecl, TypeKind};
use understanding_vocab::ontology::derived::{FoldDecl, ProtocolFoldDecl, ProtocolRuleDecl};
use understanding_vocab::ontology::OntologyPolicies;

use crate::error::{Error, Result};

/// Add the lifecycle kinds, and a rule for each pinned lever, to every
/// protocol fold of `policies` that derives one entity type's attribute from
/// one claim kind. A type the recipe already declares under a supplied name is
/// the recipe's, and is left as it is.
pub(super) fn supply(policies: &mut OntologyPolicies) -> Result<()> {
    let mut supplied = Vec::new();
    let mut rules = Vec::new();
    for (index, fold) in policies.derivation.derived.folds.iter().enumerate() {
        let Some(protocol) = &fold.protocol else {
            continue;
        };
        if !opted_in(protocol) {
            debug!(fold = %fold.id, "ontology: the recipe does not opt into lifecycle kinds");
            continue;
        }
        let Some((record, values)) = record_of(policies, fold) else {
            debug!(fold = %fold.id, "ontology: no entity attribute is derived by this fold, no lifecycle kinds");
            continue;
        };
        let kinds: BTreeSet<&str> = protocol
            .rules
            .iter()
            .map(|r| r.claim_kind.as_str())
            .collect();
        let [claim_kind] = kinds.into_iter().collect::<Vec<_>>()[..] else {
            debug!(fold = %fold.id, "ontology: the fold's rules name no one claim kind, no lifecycle kinds");
            continue;
        };
        for (lever, state) in [
            ("replaced", &protocol.replaced),
            ("acts_under", &protocol.acts_under),
        ] {
            if let Some(state) = state.as_deref().filter(|s| !values.iter().any(|v| v == s)) {
                return Err(Error::Recipe(format!(
                    "[enrichment.ontology.folds] `{}`: {lever} = \"{state}\" is not one of the states it derives ({})",
                    fold.id,
                    values.join(" | ")
                )));
            }
        }
        let field = &protocol.identity;
        let [replaced, act] = kind_names(&record);
        let ending = format!(
            "a statement that this {record} is replaced by, rolled into or superseded by another {record}, so the one replaced has ended; written about the one replaced, even when nothing else in the window speaks of it"
        );
        let doing = format!(
            "something done under a {record} as it stands: carrying it out, scheduling it or reporting on it"
        );
        let mut replaced_decl = claim(
            &replaced,
            &record,
            match &protocol.replaced {
                None => format!("{ending}, with the {field} it ends in"),
                Some(_) => ending,
            },
        );
        replaced_decl.specializes = protocol.replaced.is_none().then(|| claim_kind.to_string());
        let kinds = [
            replaced_decl,
            claim(
                &act,
                &record,
                match &protocol.acts_under {
                    None => format!("{doing}, that does not change its {field}"),
                    Some(_) => doing,
                },
            ),
        ];
        for decl in kinds {
            if policies.type_decl(&decl.name).is_some() {
                debug!(fold = %fold.id, kind = %decl.name, "ontology: the recipe declares this lifecycle kind itself");
                continue;
            }
            supplied.push(decl);
        }
        for (kind, state) in [
            (&replaced, &protocol.replaced),
            (&act, &protocol.acts_under),
        ] {
            if let Some(state) = state {
                rules.push((index, pinned(kind, state)));
            }
        }
        info!(fold = %fold.id, record = %record, claim_kind, replaced = ?protocol.replaced, acts_under = ?protocol.acts_under, "ontology: the method's lifecycle kinds");
    }
    policies.shape.types.extend(supplied);
    for (index, rule) in rules {
        if let Some(protocol) = policies.derivation.derived.folds[index].protocol.as_mut() {
            protocol.rules.push(rule);
        }
    }
    Ok(())
}

/// Whether `protocol`'s recipe asks for the lifecycle kinds: `lifecycle =
/// true`, or a pinned `replaced` or `acts_under`; `lifecycle = false` never.
pub(crate) fn opted_in(protocol: &ProtocolFoldDecl) -> bool {
    protocol.lifecycle != Some(false)
        && (protocol.lifecycle == Some(true)
            || protocol.replaced.is_some()
            || protocol.acts_under.is_some())
}

/// The lifecycle kinds' names for `fold`'s record, replaced then act-under:
/// the one decider of them, for `supply` and `recipe validate`'s note.
pub(crate) fn names(policies: &OntologyPolicies, fold: &FoldDecl) -> Option<[String; 2]> {
    fold.protocol.as_ref()?;
    Some(kind_names(&record_of(policies, fold)?.0))
}

fn kind_names(record: &str) -> [String; 2] {
    [format!("{record}_replaced"), format!("{record}_act_under")]
}

/// The entity type whose attribute `fold` derives, and the states that
/// attribute lists.
fn record_of(policies: &OntologyPolicies, fold: &FoldDecl) -> Option<(String, Vec<String>)> {
    policies
        .shape
        .types
        .iter()
        .filter(|t| t.kind == TypeKind::Entity)
        .find_map(|t| {
            let attr = t
                .attributes
                .iter()
                .find(|a| a.derived.as_deref() == Some(fold.id.as_str()))?;
            let values = match &attr.family {
                AttrFamily::Text { values } => values.clone(),
                _ => Vec::new(),
            };
            Some((t.name.clone(), values))
        })
}

fn claim(name: &str, subject: &str, description: String) -> OntologyTypeDecl {
    OntologyTypeDecl {
        name: name.to_string(),
        kind: TypeKind::Claim,
        description,
        force: Some(Force::Assertive),
        subject: Some(subject.to_string()),
        ..Default::default()
    }
}

/// A pinned lever's rule: every statement of `kind` moves its record to
/// `state`, identified by the state it enters (`protocol.rs`).
fn pinned(kind: &str, state: &str) -> ProtocolRuleDecl {
    ProtocolRuleDecl {
        id: kind.to_string(),
        claim_kind: kind.to_string(),
        state: state.to_string(),
        when: Default::default(),
        corrects: None,
        by: None,
        from: Vec::new(),
    }
}
