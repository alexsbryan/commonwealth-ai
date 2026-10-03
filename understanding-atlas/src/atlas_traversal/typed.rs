// SPDX-License-Identifier: AGPL-3.0-or-later
//! Typed query — the ONE decider for "which atoms of a declared type".
//!
//! The keyword classifier mints two declared-type plans, `Enumerate` ("which
//! coins are in this catalogue") and `Aggregate` ("how many coins by metal").
//! Both construct a [`TypedQuery`] and run through [`execute`], so a listing
//! and a tally cannot answer "is this atom a coin" two different ways.

use serde::{Deserialize, Serialize};

use crate::enrichment::atlas::atoms::Entity;
use crate::enrichment::ontology::TypeIndex;

use super::engine::{AtlasView, TraversalResult};

/// A question over the atoms of one declared type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedQuery {
    /// The author's declared type; its `specializes` descendants count as it.
    pub target_type: String,
    /// What to do with the atoms that qualify.
    pub aggregate: AnswerShape,
}

/// The answer's shape over the qualifying atoms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerShape {
    /// The atoms themselves.
    None,
    /// A count of the atoms per value of one declared attribute.
    Tally(String),
}

impl TypedQuery {
    /// Every atom of `target_type` — the `Enumerate` plan.
    pub fn listing(target_type: &str) -> Self {
        Self {
            target_type: target_type.to_string(),
            aggregate: AnswerShape::None,
        }
    }

    /// The atoms of `target_type` tallied by `over` — the `Aggregate` plan.
    pub fn tally(target_type: &str, over: &str) -> Self {
        Self {
            target_type: target_type.to_string(),
            aggregate: AnswerShape::Tally(over.to_string()),
        }
    }
}

/// Run a typed query. Refuses (a miss) when the atlas declared no vocabulary:
/// "is this a coin" has no answer without the author's declaration, and
/// answering on bare `entity_type` equality would be a second, weaker one.
pub fn execute(query: &TypedQuery, atlas: AtlasView<'_>) -> TraversalResult {
    match &query.aggregate {
        AnswerShape::None => list(&query.target_type, atlas),
        AnswerShape::Tally(over) => tally(&query.target_type, over, atlas),
    }
}

/// Cap on how many atoms an enumeration or aggregation returns. Matches the
/// brief's scannability budget; `traverse_corpus_overview` uses 8 for a
/// sample, but an enumeration's whole point is completeness, so this is the
/// larger "a catalogue, not a sample" bound.
const ENUMERATE_MAX: usize = 64;

/// What a headline calls instances of a declared type: the author's `label`
/// when they declared one, else the type name. One accessor, so the
/// enumeration and the tally cannot call the same type two different things.
///
/// `label` is SINGULAR by its own contract ("what the UI calls instances of
/// this type"), and an author's noun cannot be pluralised by a rule we own —
/// so the enumeration headline names the type and then counts
/// (`coin: 7 in this atlas`) rather than trying to agree in number. Until
/// 2026-09-03 it read `7 coin in this atlas` for every shipped template; the
/// only test that covered it declared a plural `label` no template carries.
fn declared_label(index: &TypeIndex, entity_type: &str) -> String {
    index
        .get(entity_type)
        .and_then(|d| d.label.clone())
        .unwrap_or_else(|| entity_type.to_string())
}

/// Every Entity of a declared type, including its `specializes` descendants.
///
/// This is why an enumeration of `coin` returns the sceattas too: the atlas
/// stores each atom under its OWN declared subtype, and `sceatta specializes
/// coin` is what makes a sceatta a coin. The walk goes through
/// [`TypeIndex::is_a`] — the one place the chain is walked.
fn list(entity_type: &str, atlas: AtlasView<'_>) -> TraversalResult {
    let Some(policies) = atlas.vocab else {
        // Unreachable via `classify_query_with` (the plan is only minted when
        // a vocabulary exists), but a hand-built plan must refuse rather than
        // silently enumerate on equality alone.
        return TraversalResult::miss(
            "enumerate",
            format!("No declared ontology in this atlas, so '{entity_type}' names no type."),
        );
    };
    let index = TypeIndex::from_policies(policies);
    let mut matched: Vec<Entity> = atlas
        .entities
        .iter()
        .filter(|e| index.is_a(e.entity_type.as_str_repr(), entity_type))
        .cloned()
        .collect();
    let total = matched.len();
    matched.sort_by(|a, b| {
        b.salience
            .partial_cmp(&a.salience)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.as_str().cmp(b.id.as_str()))
    });
    matched.truncate(ENUMERATE_MAX);

    tracing::debug!(
        entity_type,
        total,
        returned = matched.len(),
        "atlas traversal: enumerate over declared type"
    );

    if matched.is_empty() {
        return TraversalResult::miss(
            "enumerate",
            format!("No {entity_type} atoms in this atlas."),
        );
    }
    let mut result = TraversalResult::hit(
        "enumerate",
        format!(
            "{}: {total} in this atlas",
            declared_label(&index, entity_type)
        ),
    );
    result.entities = matched;
    result
}

/// Tally the declared type's atoms by one of its declared attributes.
///
/// Entities and Claims both carry `attributes`, and a declared claim type is
/// as tallyable as a declared entity type ("how many attributions by grade"),
/// so both are walked. An atom missing the attribute is counted under
/// `(unset)` rather than dropped — an absence is reported, never defaulted.
fn tally(entity_type: &str, over: &str, atlas: AtlasView<'_>) -> TraversalResult {
    let Some(policies) = atlas.vocab else {
        return TraversalResult::miss(
            "aggregate",
            format!("No declared ontology in this atlas, so '{entity_type}' names no type."),
        );
    };
    let index = TypeIndex::from_policies(policies);
    const UNSET: &str = "(unset)";

    let mut tally: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut bucket = |attrs: &serde_json::Map<String, serde_json::Value>| {
        let key = match attrs.get(over) {
            Some(serde_json::Value::String(s)) => match s.trim() {
                "" => UNSET.to_string(),
                t => t.to_string(),
            },
            Some(serde_json::Value::Null) | None => UNSET.to_string(),
            Some(v) => v.to_string(),
        };
        *tally.entry(key).or_insert(0) += 1;
    };

    let mut result = TraversalResult::hit("aggregate", String::new());
    for e in atlas.entities {
        if index.is_a(e.entity_type.as_str_repr(), entity_type) {
            bucket(&e.attributes);
            result.entities.push(e.clone());
        }
    }
    for c in atlas.claims {
        let subtype = c.claim_kind.as_deref().unwrap_or_default();
        if index.is_a(subtype, entity_type) {
            bucket(&c.attributes);
            result.claims.push(c.clone());
        }
    }

    let total: usize = tally.values().sum();
    tracing::debug!(
        entity_type,
        over,
        total,
        buckets = tally.len(),
        "atlas traversal: aggregate over declared attribute"
    );
    if total == 0 {
        return TraversalResult::miss(
            "aggregate",
            format!("No {entity_type} atoms in this atlas to tally by {over}."),
        );
    }
    let breakdown = tally
        .iter()
        .map(|(k, n)| format!("{k}: {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    result.headline = format!(
        "{total} {} by {over} — {breakdown}",
        declared_label(&index, entity_type)
    );
    result.entities.truncate(ENUMERATE_MAX);
    result.claims.truncate(ENUMERATE_MAX);
    result
}
