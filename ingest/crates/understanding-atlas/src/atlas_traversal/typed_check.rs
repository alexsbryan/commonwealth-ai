// SPDX-License-Identifier: AGPL-3.0-or-later
//! The typed query, checked: every name it uses found in the declared
//! vocabulary (or refused with a reason), each filter's operand read once, and
//! how a time or quantity value reads as numbers. Split from
//! [`super::typed_match`], which judges atoms against what this produces.

use std::sync::OnceLock;

use regex::Regex;

use crate::enrichment::atlas::fold;
use crate::enrichment::ontology::{AttrFamily, TypeIndex, TypeKind};

use super::typed::{AnswerShape, AttrFilter, FilterOp, Scalar, TypedQuery, Where};
use super::typed_match::Judge;

/// How a link between two declared types is carried.
#[derive(Clone, Copy, Debug)]
pub(super) enum Link<'q> {
    /// A Relation atom of this declared relation type (or a subtype).
    Relation(&'q str),
    /// The atom's own ref attribute holds the far end.
    RefOut(&'q str),
    /// The far end's ref attribute holds the atom.
    RefIn(&'q str),
}

/// How a filtered attribute reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ValueKind {
    Name,
    Text,
    Time,
    Quantity,
}

pub(super) enum Operand {
    /// Folded text.
    Text(String),
    Num(f64),
}

pub(super) struct CheckedFilter<'q> {
    pub(super) attribute: &'q str,
    pub(super) op: FilterOp,
    pub(super) negate: bool,
    pub(super) kind: ValueKind,
    pub(super) operand: Operand,
}

pub(super) struct CheckedRelation<'q> {
    pub(super) relation: &'q str,
    pub(super) other_type: &'q str,
    pub(super) other_name: Option<&'q str>,
    pub(super) negate: bool,
    pub(super) link: Link<'q>,
    pub(super) within: Option<CheckedWhere<'q>>,
}

pub(super) struct CheckedWhere<'q> {
    pub(super) filters: Vec<CheckedFilter<'q>>,
    pub(super) relations: Vec<CheckedRelation<'q>>,
}

/// What an `argmax`/`argmin` ranks by.
pub(super) enum Over<'q> {
    /// A time or quantity attribute of the target.
    Attr(&'q str, ValueKind),
    /// A related type, through every declared link between it and the target.
    Related(&'q str, Vec<Link<'q>>),
}

impl Over<'_> {
    pub(super) fn name(&self) -> &str {
        match self {
            Over::Attr(n, _) | Over::Related(n, _) => n,
        }
    }
}

pub(super) enum Shape<'q> {
    List,
    Count,
    Tally(&'q str),
    Best { highest: bool, over: Over<'q> },
}

/// A query whose every name was found in the declared vocabulary.
pub(super) struct Checked<'q> {
    pub(super) target: &'q str,
    pub(super) filters: Vec<CheckedFilter<'q>>,
    pub(super) relations: Vec<CheckedRelation<'q>>,
    pub(super) shape: Shape<'q>,
}

impl<'q> Checked<'q> {
    /// Does any relation constraint, at either level, ask for an absent link?
    pub(super) fn negates_a_link(&self) -> bool {
        self.relations.iter().any(|r| {
            r.negate
                || r.within
                    .as_ref()
                    .is_some_and(|w| w.relations.iter().any(|h| h.negate))
        })
    }

    /// The target's attributes the query names: its filters' (bar `name`),
    /// a tally's, and an attribute an argmax/argmin ranks by.
    pub(super) fn named_attributes(&self) -> Vec<&'q str> {
        let mut out: Vec<&'q str> = Vec::new();
        let over = match &self.shape {
            Shape::Tally(a)
            | Shape::Best {
                over: Over::Attr(a, _),
                ..
            } => Some(*a),
            _ => None,
        };
        let filtered = self
            .filters
            .iter()
            .filter(|f| f.kind != ValueKind::Name)
            .map(|f| f.attribute);
        for a in filtered.chain(over) {
            if !out.contains(&a) {
                out.push(a);
            }
        }
        out
    }
}

/// Check every name in the query against the declared vocabulary. The first
/// one it does not hold is the refusal, worded for the person who wrote it.
pub(super) fn check<'q>(q: &'q TypedQuery, judge: &Judge<'q>) -> Result<Checked<'q>, String> {
    let index = &judge.index;
    let t = q.target_type.as_str();
    let decl = index
        .get(t)
        .ok_or_else(|| format!("'{t}' is not a declared type in this atlas."))?;
    if !matches!(decl.kind, TypeKind::Entity | TypeKind::Claim) {
        return Err(format!(
            "'{t}' is a {:?} type; a typed query answers over entity or claim types.",
            decl.kind
        ));
    }
    let filters = q
        .filters
        .iter()
        .map(|f| check_filter(f, t, index))
        .collect::<Result<Vec<_>, _>>()?;
    let mut relations = Vec::new();
    for r in &q.relations {
        let mut c = check_relation(
            &r.relation,
            &r.other_type,
            r.other_name.as_deref(),
            r.negate,
            t,
            index,
        )?;
        c.within = r
            .within
            .as_ref()
            .map(|w| check_where(w, &r.other_type, index))
            .transpose()?;
        relations.push(c);
    }
    let shape = match &q.aggregate {
        AnswerShape::None => Shape::List,
        AnswerShape::Count => Shape::Count,
        AnswerShape::Tally(over) => {
            if !index
                .effective_attributes(t)
                .iter()
                .any(|a| &a.name == over)
            {
                return Err(format!(
                    "'{over}' is not an attribute of {t}, so it cannot be tallied."
                ));
            }
            Shape::Tally(over)
        }
        AnswerShape::Argmax(over) => Shape::Best {
            highest: true,
            over: check_over(over, t, judge)?,
        },
        AnswerShape::Argmin(over) => Shape::Best {
            highest: false,
            over: check_over(over, t, judge)?,
        },
    };
    Ok(Checked {
        target: t,
        filters,
        relations,
        shape,
    })
}

fn check_filter<'q>(
    f: &'q AttrFilter,
    t: &str,
    index: &TypeIndex<'_>,
) -> Result<CheckedFilter<'q>, String> {
    let a = f.attribute.as_str();
    let kind = if a == "name" {
        ValueKind::Name
    } else {
        let decl = index
            .effective_attributes(t)
            .into_iter()
            .find(|d| d.name == a)
            .ok_or_else(|| format!("'{a}' is not an attribute of {t}."))?;
        match &decl.family {
            AttrFamily::Text { .. } => ValueKind::Text,
            AttrFamily::Time { .. } => ValueKind::Time,
            AttrFamily::Quantity { .. } => ValueKind::Quantity,
            AttrFamily::Ref { .. } => {
                return Err(format!(
                    "'{a}' is a ref; constrain it as the relation '{t}.{a}'."
                ))
            }
        }
    };
    let fits = match kind {
        ValueKind::Name | ValueKind::Text => matches!(f.op, FilterOp::Eq | FilterOp::Contains),
        ValueKind::Time | ValueKind::Quantity => f.op != FilterOp::Contains,
    };
    if !fits {
        return Err(format!("'{a}' cannot be compared by {:?}.", f.op));
    }
    let text = match &f.value {
        Scalar::Text(s) => s.clone(),
        Scalar::Number(n) => n.to_string(),
    };
    let operand = match kind {
        ValueKind::Name | ValueKind::Text => Operand::Text(fold(&text)),
        ValueKind::Time => Operand::Num(
            match &f.value {
                Scalar::Number(n) => n.as_f64(),
                Scalar::Text(s) => signed_years(s).first().copied(),
            }
            .ok_or_else(|| format!("'{text}' does not read as a year."))?,
        ),
        ValueKind::Quantity => Operand::Num(
            numbers(&text)
                .first()
                .copied()
                .ok_or_else(|| format!("'{text}' does not read as a number."))?,
        ),
    };
    Ok(CheckedFilter {
        attribute: a,
        op: f.op,
        negate: f.negate,
        kind,
        operand,
    })
}

fn check_relation<'q>(
    relation: &'q str,
    other: &'q str,
    other_name: Option<&'q str>,
    negate: bool,
    t: &str,
    index: &TypeIndex<'_>,
) -> Result<CheckedRelation<'q>, String> {
    if !index.contains(other) {
        return Err(format!("'{other}' is not a declared type in this atlas."));
    }
    let link = if index
        .get(relation)
        .is_some_and(|d| d.kind == TypeKind::Relation)
    {
        let [from, to] = index.endpoints(relation);
        let fits = |x: &str, end: Option<&str>| end.is_none_or(|e| index.is_a(x, e));
        if (fits(t, from) && fits(other, to)) || (fits(t, to) && fits(other, from)) {
            Link::Relation(relation)
        } else {
            return Err(format!(
                "relation '{relation}' joins {} and {}, not {t} and {other}.",
                from.unwrap_or("any type"),
                to.unwrap_or("any type")
            ));
        }
    } else if let Some((owner, attr)) = relation.split_once('.') {
        let of = ref_target(index, owner, attr)
            .ok_or_else(|| format!("'{relation}' names no ref attribute of a declared type."))?;
        if index.is_a(t, owner) && index.is_a(other, of) {
            Link::RefOut(attr)
        } else if index.is_a(t, of) && index.is_a(other, owner) {
            Link::RefIn(attr)
        } else {
            return Err(format!(
                "ref '{relation}' joins {owner} and {of}, not {t} and {other}."
            ));
        }
    } else {
        return Err(format!(
            "'{relation}' is neither a declared relation type nor a ref '<type>.<attribute>'."
        ));
    };
    Ok(CheckedRelation {
        relation,
        other_type: other,
        other_name,
        negate,
        link,
        within: None,
    })
}

fn check_where<'q>(
    w: &'q Where,
    on: &'q str,
    index: &TypeIndex<'_>,
) -> Result<CheckedWhere<'q>, String> {
    let filters = w
        .filters
        .iter()
        .map(|f| check_filter(f, on, index))
        .collect::<Result<Vec<_>, _>>()?;
    let relations = w
        .relations
        .iter()
        .map(|h| {
            check_relation(
                &h.relation,
                &h.other_type,
                h.other_name.as_deref(),
                h.negate,
                on,
                index,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CheckedWhere { filters, relations })
}

/// The type a ref attribute `owner.attr` points at, if it is one.
fn ref_target<'a>(index: &TypeIndex<'a>, owner: &str, attr: &str) -> Option<&'a str> {
    index
        .effective_attributes(owner)
        .into_iter()
        .find(|a| a.name == attr)
        .and_then(|a| match &a.family {
            AttrFamily::Ref { of } => Some(of.as_str()),
            _ => None,
        })
}

fn check_over<'q>(over: &'q str, t: &str, judge: &Judge<'q>) -> Result<Over<'q>, String> {
    let index = &judge.index;
    if let Some(decl) = index
        .effective_attributes(t)
        .into_iter()
        .find(|a| a.name == over)
    {
        return match &decl.family {
            AttrFamily::Time { .. } => Ok(Over::Attr(over, ValueKind::Time)),
            AttrFamily::Quantity { .. } => Ok(Over::Attr(over, ValueKind::Quantity)),
            other => Err(format!(
                "'{over}' is a {} attribute; argmax/argmin ranks a time or quantity attribute or a related type.",
                other.key()
            )),
        };
    }
    if !index.contains(over) {
        return Err(format!(
            "'{over}' is neither an attribute of {t} nor a declared type."
        ));
    }
    let mut links = Vec::new();
    for d in &judge.policies.shape.types {
        if d.kind == TypeKind::Relation {
            if check_relation(&d.name, over, None, false, t, index).is_ok() {
                links.push(Link::Relation(d.name.as_str()));
            }
            continue;
        }
        for a in &d.attributes {
            if let AttrFamily::Ref { of } = &a.family {
                if index.is_a(t, &d.name) && index.is_a(over, of) {
                    links.push(Link::RefOut(a.name.as_str()));
                } else if index.is_a(t, of) && index.is_a(over, &d.name) {
                    links.push(Link::RefIn(a.name.as_str()));
                }
            }
        }
    }
    if links.is_empty() {
        return Err(format!("no declared relation or ref joins {t} and {over}."));
    }
    Ok(Over::Related(over, links))
}

/// The years a time value names, in order, signed: B.C. (or BC / BCE)
/// anywhere makes every year negative. A year is a run of 1-4 digits not
/// glued to a letter ("4th", "1960s" are not years); a short run after a
/// slash completes the one before it, the scholarly span notation:
/// "317/6 B.C." is 317 and 316 B.C., "1914/15" is 1914 and 1915. Text with no
/// year ("third century B.C.") reads as nothing — unjudged, not zero.
pub(super) fn signed_years(text: &str) -> Vec<f64> {
    static DIGITS: OnceLock<Regex> = OnceLock::new();
    static BC: OnceLock<Regex> = OnceLock::new();
    let digits = DIGITS.get_or_init(|| Regex::new(r"\d+").expect("static regex"));
    let bc = BC
        .get_or_init(|| Regex::new(r"(?i)\bb\.?\s?c(?:\.?\s?e)?(?:\.|\b)").expect("static regex"));
    let sign = if bc.is_match(text) { -1.0 } else { 1.0 };
    let mut out: Vec<f64> = Vec::new();
    let mut prev: Option<(usize, String)> = None; // (end offset, digits)
    for m in digits.find_iter(text) {
        let glued = text[m.end()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic());
        let run = m.as_str();
        if glued || run.len() > 4 {
            prev = None;
            continue;
        }
        let year = match &prev {
            Some((end, p)) if text[*end..m.start()] == *"/" && run.len() < p.len() => {
                format!("{}{run}", &p[..p.len() - run.len()])
            }
            _ => run.to_string(),
        };
        if let Ok(y) = year.parse::<f64>() {
            out.push(sign * y);
        }
        prev = Some((m.end(), year));
    }
    out
}

/// The numbers a quantity value names, in order ("8.5-8.6 g" is 8.5 and 8.6).
pub(super) fn numbers(text: &str) -> Vec<f64> {
    static NUM: OnceLock<Regex> = OnceLock::new();
    NUM.get_or_init(|| Regex::new(r"\d+(?:\.\d+)?").expect("static regex"))
        .find_iter(text)
        .filter_map(|m| m.as_str().parse().ok())
        .collect()
}
