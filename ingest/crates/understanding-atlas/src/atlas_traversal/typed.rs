// SPDX-License-Identifier: AGPL-3.0-or-later
//! Typed query — the ONE decider for "which atoms of a declared type".
//!
//! The keyword classifier mints two declared-type plans, `Enumerate` ("which
//! coins are in this catalogue") and `Aggregate` ("how many coins by metal").
//! Both construct a [`TypedQuery`] and run through [`execute`], so a listing
//! and a tally cannot answer "is this atom a coin" two different ways. A query
//! handed in whole (`svrn enrich atlas-query --typed`) is the general form:
//!
//! ```json
//! {"target_type": "hoard",
//!  "filters":   [{"attribute": "findspot", "op": "contains", "value": "Egypt", "negate": false}],
//!  "relations": [{"relation": "holds_coins_of", "other_type": "mint", "other_name": "Abydus",
//!                 "negate": true, "where": null}],
//!  "aggregate": "none", "aggregate_over": null}
//! ```
//!
//! A filter compares one declared attribute (or `name`) by `eq`, `lt`, `gt` or
//! `contains`. A relation constraint asks for a far-end atom of `other_type`
//! joined by a declared relation type or a ref attribute spelled
//! `<type>.<attribute>`, optionally NAMED and optionally meeting a `where` of
//! its own filters and one-hop relations — depth 2, by the types: a `where`'s
//! relations have no `where`. `aggregate` is `none`, `count`, `argmax` /
//! `argmin` over a time or quantity attribute or a related type (the count of
//! distinct linked atoms meeting that relation's `where`), or `tally` by an
//! attribute. The shape is the one the feature-fidelity query-layer probe
//! validated (`research/ontology-retrieval/ontology-proof/ans/k2_query.py`).
//!
//! Every query is checked against the declared vocabulary before it runs, and
//! a name the vocabulary does not hold is refused, never skipped. The per-atom
//! judgements — names, intervals, links, absence — are in [`super::typed_match`],
//! the vocabulary check in [`super::typed_check`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::enrichment::ontology::TypeIndex;

use super::engine::{AtlasView, TraversalResult};
use super::typed_check::{check, Checked, Over, Shape};
use super::typed_match::{Atom, Judge, Via};

/// A question over the atoms of one declared type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "TypedQueryWire", into = "TypedQueryWire")]
pub struct TypedQuery {
    /// The author's declared type; its `specializes` descendants count as it.
    pub target_type: String,
    /// Every filter must hold.
    pub filters: Vec<AttrFilter>,
    /// Every relation constraint must hold (or, negated, must not).
    pub relations: Vec<RelationConstraint>,
    /// What to answer about the atoms that qualify.
    pub aggregate: AnswerShape,
}

/// One comparison on an attribute of the atom (or on its `name`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttrFilter {
    /// A declared attribute of the type, or `name` (canonical name or alias).
    pub attribute: String,
    pub op: FilterOp,
    pub value: Scalar,
    #[serde(default)]
    pub negate: bool,
}

/// How a filter compares. `lt`/`gt` apply to time and quantity attributes,
/// `contains` to text and `name`, `eq` to all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterOp {
    Eq,
    Lt,
    Gt,
    Contains,
}

/// A filter's operand, as JSON spells it. A time is a signed year (B.C.
/// negative), as a number or as text ("318 B.C.").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Scalar {
    Number(serde_json::Number),
    Text(String),
}

/// A far-end atom the target must (or, negated, must not) be joined to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationConstraint {
    /// A declared relation type, or a ref attribute spelled `<type>.<attr>`.
    pub relation: String,
    pub other_type: String,
    /// The far end's canonical name or alias; `None` = any.
    #[serde(default)]
    pub other_name: Option<String>,
    #[serde(default)]
    pub negate: bool,
    /// Conditions on the far end itself.
    #[serde(default, rename = "where")]
    pub within: Option<Where>,
}

/// The far end's own filters and one-hop relations — the second and last level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Where {
    #[serde(default)]
    pub filters: Vec<AttrFilter>,
    #[serde(default)]
    pub relations: Vec<Hop>,
}

/// A relation constraint inside a `where`. It has no `where` of its own: the
/// depth limit is this type's shape, so a third level does not parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hop {
    pub relation: String,
    pub other_type: String,
    #[serde(default)]
    pub other_name: Option<String>,
    #[serde(default)]
    pub negate: bool,
}

/// The answer's shape over the qualifying atoms. The `String` is the wire's
/// `aggregate_over`, carried only by the shapes that need one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerShape {
    /// The atoms themselves.
    None,
    /// How many atoms qualify (the rows still list them, cited).
    Count,
    /// The atoms with the highest value: a time or quantity attribute (an
    /// interval's END), or a related type (how many linked atoms).
    Argmax(String),
    /// The lowest: an interval's START, or the fewest linked atoms.
    Argmin(String),
    /// A count of the atoms per value of one declared attribute.
    Tally(String),
}

/// `aggregate` + `aggregate_over` as JSON spells them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TypedQueryWire {
    target_type: String,
    #[serde(default)]
    filters: Vec<AttrFilter>,
    #[serde(default)]
    relations: Vec<RelationConstraint>,
    #[serde(default)]
    aggregate: AggregateWord,
    #[serde(default)]
    aggregate_over: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AggregateWord {
    #[default]
    None,
    Count,
    Argmax,
    Argmin,
    Tally,
}

impl TryFrom<TypedQueryWire> for TypedQuery {
    type Error = String;

    /// An `aggregate_over` the shape does not use, or a missing one it needs,
    /// is refused here rather than ignored or defaulted.
    fn try_from(w: TypedQueryWire) -> Result<Self, String> {
        use AggregateWord as A;
        let aggregate = match (w.aggregate, w.aggregate_over) {
            (A::None, None) => AnswerShape::None,
            (A::Count, None) => AnswerShape::Count,
            (A::Argmax, Some(o)) => AnswerShape::Argmax(o),
            (A::Argmin, Some(o)) => AnswerShape::Argmin(o),
            (A::Tally, Some(o)) => AnswerShape::Tally(o),
            (word @ (A::None | A::Count), Some(o)) => {
                return Err(format!(
                    "aggregate {word:?} takes no aggregate_over (got '{o}')"
                ))
            }
            (word, None) => return Err(format!("aggregate {word:?} needs an aggregate_over")),
        };
        Ok(TypedQuery {
            target_type: w.target_type,
            filters: w.filters,
            relations: w.relations,
            aggregate,
        })
    }
}

impl From<TypedQuery> for TypedQueryWire {
    fn from(q: TypedQuery) -> Self {
        let (aggregate, aggregate_over) = match q.aggregate {
            AnswerShape::None => (AggregateWord::None, None),
            AnswerShape::Count => (AggregateWord::Count, None),
            AnswerShape::Argmax(o) => (AggregateWord::Argmax, Some(o)),
            AnswerShape::Argmin(o) => (AggregateWord::Argmin, Some(o)),
            AnswerShape::Tally(o) => (AggregateWord::Tally, Some(o)),
        };
        TypedQueryWire {
            target_type: q.target_type,
            filters: q.filters,
            relations: q.relations,
            aggregate,
            aggregate_over,
        }
    }
}

impl TypedQuery {
    /// Every atom of `target_type` — the `Enumerate` plan.
    pub fn listing(target_type: &str) -> Self {
        Self {
            target_type: target_type.to_string(),
            filters: Vec::new(),
            relations: Vec::new(),
            aggregate: AnswerShape::None,
        }
    }

    /// The atoms of `target_type` tallied by `over` — the `Aggregate` plan.
    pub fn tally(target_type: &str, over: &str) -> Self {
        Self {
            aggregate: AnswerShape::Tally(over.to_string()),
            ..Self::listing(target_type)
        }
    }

    /// Does the query constrain the set at all? An unconstrained listing is
    /// the catalogue ("coin: 7 in this atlas"); a constrained one is an answer.
    fn constrained(&self) -> bool {
        !self.filters.is_empty() || !self.relations.is_empty()
    }
}

/// The answer as a table: one row per answer atom, each cited.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TypedTable {
    /// Atoms of the type that met every filter and relation constraint. For
    /// `count` this IS the answer; for `argmax`/`argmin` it is the field the
    /// winners were picked from.
    pub matched: usize,
    /// The answer atoms (every match, or the winners), capped at
    /// [`ENUMERATE_MAX`] — a note says so when the cap bites.
    pub rows: Vec<TypedRow>,
    /// What the answer could not judge or had to leave out, in words: an
    /// unresolved name, atoms set aside for an unset attribute, the cap.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// One answer atom.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TypedRow {
    pub name: String,
    pub atom_id: String,
    /// The atom's own attributes, plus every attribute the query names that
    /// the atom does not carry, as `null` — absence shown, never filled in.
    pub attributes: serde_json::Map<String, Value>,
    /// Chunk ids: the atom's own evidence, then that of every atom that
    /// carried a satisfied relation constraint (the Relation atom, or the
    /// atom holding the ref) — so each row cites why it is in the answer.
    pub evidence: Vec<String>,
}

/// Run a typed query. Refuses (a miss) when the atlas declared no vocabulary:
/// "is this a coin" has no answer without the author's declaration, and
/// answering on bare `entity_type` equality would be a second, weaker one.
pub fn execute(query: &TypedQuery, atlas: AtlasView<'_>) -> TraversalResult {
    let kind = match query.aggregate {
        AnswerShape::Tally(_) => "aggregate",
        _ => "enumerate",
    };
    let target = query.target_type.as_str();
    let Some(policies) = atlas.vocab else {
        // Unreachable via `classify_query_with` (the plan is only minted when
        // a vocabulary exists), but a hand-built plan must refuse rather than
        // silently enumerate on equality alone.
        return TraversalResult::miss(
            kind,
            format!("No declared ontology in this atlas, so '{target}' names no type."),
        );
    };
    let judge = Judge::new(atlas, policies);
    let checked = match check(query, &judge) {
        Ok(c) => c,
        Err(why) => {
            tracing::debug!(target_type = target, %why, "atlas traversal: typed query refused");
            return TraversalResult::miss(kind, why);
        }
    };

    let candidates = judge.of_type(target);
    if candidates.is_empty() {
        tracing::debug!(
            target_type = target,
            "atlas traversal: typed query has no atoms of its type"
        );
        let why = match &query.aggregate {
            AnswerShape::Tally(over) => {
                format!("No {target} atoms in this atlas to tally by {over}.")
            }
            _ => format!("No {target} atoms in this atlas."),
        };
        return TraversalResult::miss(kind, why);
    }

    let mut notes = judge.unresolved_names(&checked);
    if checked.negates_a_link() {
        // Closed world, said out loud: "holds no Abydus coins" means no
        // RECORDED link, which is weaker than the text saying so.
        notes.push(
            "a negated relation is judged over the links this atlas records; an unrecorded link counts as absent."
                .to_string(),
        );
    }
    let mut matched: Vec<(Atom<'_>, Vec<Via<'_>>)> = candidates
        .iter()
        .filter_map(|&atom| judge.qualifies(atom, &checked).map(|v| (atom, v)))
        .collect();
    matched.sort_by(|a, b| Atom::rank(a.0, b.0));
    let total = matched.len();
    tracing::debug!(
        target_type = target,
        candidates = candidates.len(),
        matched = total,
        "atlas traversal: typed query matched"
    );

    let label = declared_label(&judge.index, target);
    let (headline, mut answer) = match &checked.shape {
        Shape::List | Shape::Count => {
            let headline = if query.constrained() {
                format!("{label}: {total} match")
            } else {
                format!("{label}: {total} in this atlas")
            };
            (headline, matched)
        }
        Shape::Tally(over) => (tally_headline(&matched, over, &label), matched),
        Shape::Best { highest, over } => {
            let (winners, unscored) = best(&judge, &checked, matched, *highest, over);
            if unscored > 0 {
                notes.push(format!(
                    "{unscored} matching {target} atom(s) carry no readable {}, so were not ranked.",
                    over.name()
                ));
            }
            let word = match (over, highest) {
                (Over::Attr(..), true) => "highest",
                (Over::Attr(..), false) => "lowest",
                (Over::Related(..), true) => "most linked",
                (Over::Related(..), false) => "fewest linked",
            };
            let headline = format!(
                "{label}: {} with the {word} {} of {total} that match",
                winners.len(),
                over.name()
            );
            (headline, winners)
        }
    };

    if answer.len() > ENUMERATE_MAX {
        notes.push(format!(
            "showing the first {ENUMERATE_MAX} of {} by salience.",
            answer.len()
        ));
        answer.truncate(ENUMERATE_MAX);
    }
    notes.extend(judge.unjudged_notes());

    let named = checked.named_attributes();
    let mut result = TraversalResult::hit(kind, headline);
    let rows = answer
        .iter()
        .map(|(atom, via)| row(*atom, via, &named))
        .collect();
    for (atom, _) in &answer {
        match atom {
            Atom::Entity(e) => result.entities.push((*e).clone()),
            Atom::Claim(c) => result.claims.push((*c).clone()),
        }
    }
    result.table = Some(TypedTable {
        matched: total,
        rows,
        notes,
    });
    result
}

/// Cap on how many atoms an enumeration or aggregation returns. Matches the
/// brief's scannability budget; `traverse_corpus_overview` uses 8 for a
/// sample, but an enumeration's whole point is completeness, so this is the
/// larger "a catalogue, not a sample" bound. `TypedTable::matched` keeps the
/// full count, and a note names the cap whenever it bites.
pub const ENUMERATE_MAX: usize = 64;

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

/// The tally headline. An atom missing the attribute is counted under
/// `(unset)` rather than dropped — an absence is reported, never defaulted.
fn tally_headline(matched: &[(Atom<'_>, Vec<Via<'_>>)], over: &str, label: &str) -> String {
    const UNSET: &str = "(unset)";
    let mut tally: BTreeMap<String, usize> = BTreeMap::new();
    for (atom, _) in matched {
        let key = match atom.attributes().get(over) {
            Some(Value::String(s)) => match s.trim() {
                "" => UNSET.to_string(),
                t => t.to_string(),
            },
            Some(Value::Null) | None => UNSET.to_string(),
            Some(v) => v.to_string(),
        };
        *tally.entry(key).or_insert(0) += 1;
    }
    let breakdown = tally
        .iter()
        .map(|(k, n)| format!("{k}: {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    tracing::debug!(
        over,
        total = matched.len(),
        buckets = tally.len(),
        "atlas traversal: aggregate over declared attribute"
    );
    format!("{} {label} by {over} — {breakdown}", matched.len())
}

/// The winners of an `argmax`/`argmin`, every tie kept, and how many matching
/// atoms had no score at all (an unset or unreadable attribute) — those are
/// set aside and counted, never scored as zero.
///
/// An interval scores by its END for argmax and its START for argmin, so the
/// latest burial is the one that could have closed last and the earliest the
/// one that could have closed first. A related type scores by how many
/// distinct linked atoms of it meet every `where` the query put on that type;
/// an argmax of zero links names nothing, so zero counts drop out of argmax.
fn best<'a>(
    judge: &Judge<'a>,
    checked: &Checked<'_>,
    matched: Vec<(Atom<'a>, Vec<Via<'a>>)>,
    highest: bool,
    over: &Over<'_>,
) -> (Vec<(Atom<'a>, Vec<Via<'a>>)>, usize) {
    let mut scored: Vec<(f64, Atom<'a>, Vec<Via<'a>>)> = Vec::new();
    let mut unscored = 0usize;
    for (atom, mut via) in matched {
        let score = match over {
            Over::Attr(name, kind) => {
                judge
                    .interval(atom, name, *kind)
                    .map(|(lo, hi)| if highest { hi } else { lo })
            }
            Over::Related(other, links) => {
                let linked = judge.linked_in_scope(atom, other, links, checked);
                let n = linked.len();
                via.extend(linked.into_iter().flat_map(|(_, v)| v));
                Some(n as f64)
            }
        };
        match score {
            Some(s) if highest && s == 0.0 && matches!(over, Over::Related(..)) => {
                tracing::debug!(
                    atom = atom.id(),
                    "atlas traversal: argmax drops an atom with no linked atom"
                );
            }
            Some(s) => scored.push((s, atom, via)),
            None => {
                tracing::debug!(
                    atom = atom.id(),
                    over = over.name(),
                    "atlas traversal: argmax/argmin cannot score atom"
                );
                unscored += 1;
            }
        }
    }
    let scores = scored.iter().map(|(s, _, _)| *s);
    let pick = if highest {
        scores.reduce(f64::max)
    } else {
        scores.reduce(f64::min)
    };
    let winners: Vec<_> = match pick {
        Some(p) => scored
            .into_iter()
            .filter(|(s, _, _)| *s == p)
            .map(|(_, a, v)| (a, v))
            .collect(),
        None => Vec::new(),
    };
    tracing::debug!(
        over = over.name(),
        highest,
        winners = winners.len(),
        unscored,
        "atlas traversal: argmax/argmin selected"
    );
    (winners, unscored)
}

/// One cited row. Every attribute the query names that the atom lacks is
/// added as `null`, so the row shows the absence rather than hiding it.
fn row(atom: Atom<'_>, via: &[Via<'_>], named: &[&str]) -> TypedRow {
    let mut attributes = atom.attributes().clone();
    for a in named {
        attributes.entry(a.to_string()).or_insert(Value::Null);
    }
    let mut evidence: Vec<String> = Vec::new();
    for id in atom
        .chunk_ids()
        .into_iter()
        .chain(via.iter().flat_map(|v| v.chunk_ids()))
    {
        if !evidence.contains(&id) {
            evidence.push(id);
        }
    }
    TypedRow {
        name: atom.name().to_string(),
        atom_id: atom.id().to_string(),
        attributes,
        evidence,
    }
}

#[cfg(test)]
#[path = "tests/typed.rs"]
mod tests;
