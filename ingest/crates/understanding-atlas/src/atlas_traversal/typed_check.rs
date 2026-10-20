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

/// `<claim type>.subject`: a claim's link to the record it is about, which the
/// claim holds beside its attributes rather than as one.
pub(super) const SUBJECT: &str = "subject";

/// How a link between two declared types is carried.
#[derive(Clone, Copy, Debug)]
pub(super) enum Link<'q> {
    /// A Relation atom of this declared relation type (or a subtype).
    Relation(&'q str),
    /// The atom's own ref attribute (or a claim's `subject`) holds the far end.
    RefOut(&'q str),
    /// The far end's ref attribute (or a claim's `subject`) holds the atom.
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
    /// A time bound, `[start, end)` on the signed-year axis (`time_spans`).
    Span(f64, f64),
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

/// What a tally or a sum groups by: an attribute of the target, or its
/// `subject`. A ref's group is the record it holds, named by that record.
pub(super) struct Group<'q> {
    pub(super) attr: &'q str,
    pub(super) is_ref: bool,
}

pub(super) enum Shape<'q> {
    List,
    Count,
    Tally(Group<'q>),
    Best {
        highest: bool,
        over: Over<'q>,
    },
    /// The total of a quantity attribute, per group or (`None`) in one.
    Sum {
        over: &'q str,
        by: Option<Group<'q>>,
    },
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
    /// a tally's, an attribute an argmax/argmin ranks by, and a sum's.
    pub(super) fn named_attributes(&self) -> Vec<&'q str> {
        let mut out: Vec<&'q str> = Vec::new();
        let over: Vec<&'q str> = match &self.shape {
            Shape::Tally(g) => vec![g.attr],
            Shape::Best {
                over: Over::Attr(a, _),
                ..
            } => vec![*a],
            Shape::Sum { over, by } => std::iter::once(*over)
                .chain(by.as_ref().map(|g| g.attr))
                .collect(),
            _ => Vec::new(),
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
    // A declared event type is listable as entities and claims are (a novel's
    // incidents: saltgrass, 2026-10-14). A state or relation the document
    // reader reads is a cited statement about its holder, stored as a claim
    // of its type, so it is listed as claims are; the general extractor's
    // State and Relation atoms carry no declared attributes and are not.
    let listable = match decl.kind {
        TypeKind::Entity | TypeKind::Event | TypeKind::Claim => true,
        TypeKind::State | TypeKind::Relation => judge.policies.reads_documents(),
    };
    if !listable {
        return Err(format!(
            "'{t}' is a {:?} type the general extractor reads; a typed query answers over entity, event or claim types, and over states and relations the document reader reads.",
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
        AnswerShape::Tally(over) => Shape::Tally(check_group(over, t, index, "tallied")?),
        AnswerShape::Sum { over, by } => Shape::Sum {
            over: check_summand(over, t, index)?,
            by: by
                .as_deref()
                .map(|g| check_group(g, t, index, "grouped by"))
                .transpose()?,
        },
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

/// A tally's or a sum's group: a declared attribute of `t` (a ref groups by
/// the record it holds), or `subject` when `t` declares what it is about.
fn check_group<'q>(
    over: &'q str,
    t: &str,
    index: &TypeIndex<'_>,
    verb: &str,
) -> Result<Group<'q>, String> {
    if let Some(a) = index
        .effective_attributes(t)
        .into_iter()
        .find(|a| a.name == over)
    {
        let is_ref = matches!(a.family, AttrFamily::Ref { .. });
        return Ok(Group { attr: over, is_ref });
    }
    if over == SUBJECT && ref_target(index, t, SUBJECT).is_some() {
        tracing::trace!(
            target_type = t,
            "atlas traversal: typed query groups by a claim's subject"
        );
        return Ok(Group {
            attr: over,
            is_ref: true,
        });
    }
    Err(format!(
        "'{over}' is not an attribute of {t}, so it cannot be {verb}."
    ))
}

/// What a sum adds up: a quantity attribute of `t`, and nothing else.
fn check_summand<'q>(over: &'q str, t: &str, index: &TypeIndex<'_>) -> Result<&'q str, String> {
    let decl = index
        .effective_attributes(t)
        .into_iter()
        .find(|a| a.name == over)
        .ok_or_else(|| format!("'{over}' is not an attribute of {t}, so it cannot be summed."))?;
    match &decl.family {
        AttrFamily::Quantity { .. } => Ok(over),
        other => Err(format!(
            "'{over}' is a {} attribute; sum adds up a quantity attribute.",
            other.key()
        )),
    }
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
        ValueKind::Time => {
            let (start, end) = match &f.value {
                Scalar::Number(n) => n.as_f64().map(|y| (y, y + 1.0)),
                Scalar::Text(s) => time_spans(s).first().copied(),
            }
            .ok_or_else(|| format!("'{text}' does not read as a date or a year."))?;
            Operand::Span(start, end)
        }
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
/// The type `<owner>.<attr>` points at: a declared ref attribute's `of`, else,
/// for `subject`, the claim type's declared subject (the is-about link a claim
/// holds beside its attributes, `Atom::held`).
fn ref_target<'a>(index: &TypeIndex<'a>, owner: &str, attr: &str) -> Option<&'a str> {
    let declared = index
        .effective_attributes(owner)
        .into_iter()
        .find(|a| a.name == attr);
    match declared {
        Some(a) => match &a.family {
            AttrFamily::Ref { of } => Some(of.as_str()),
            _ => None,
        },
        None if attr == SUBJECT => index.get(owner).and_then(|d| d.subject.as_deref()),
        None => None,
    }
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

/// The spans a time value names on the signed-year axis, each half-open
/// `[start, end)`: a date is its day (`2026-10-20`, `20 October 2026`,
/// `October 20, 2026`), a month its month (`2026-10`, `October 2026`), and
/// anything else its years (`signed_years`), each `[y, y + 1)`. Business
/// records date to the day; read as bare years, `2026-10-20` was the years
/// 2026, 10 and 20.
pub(super) fn time_spans(text: &str) -> Vec<(f64, f64)> {
    static ISO: OnceLock<Regex> = OnceLock::new();
    static WORDS: OnceLock<Regex> = OnceLock::new();
    let iso = ISO
        .get_or_init(|| Regex::new(r"\b(\d{4})-(\d{1,2})(?:-(\d{1,2}))?\b").expect("static regex"));
    let words = WORDS.get_or_init(|| {
        Regex::new(
            r"(?i)\b(?:(\d{1,2})\s+)?(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*\.?(?:\s+(\d{1,2}))?,?\s+(\d{4})\b",
        )
        .expect("static regex")
    });
    let mut out: Vec<(f64, f64)> = iso
        .captures_iter(text)
        .filter_map(|c| {
            let num = |i: usize| c.get(i).and_then(|m| m.as_str().parse::<u32>().ok());
            let year = num(1)?;
            calendar_span(year as i32, num(2)?, num(3))
        })
        .collect();
    if out.is_empty() {
        out = words
            .captures_iter(text)
            .filter_map(|c| {
                const MONTHS: [&str; 12] = [
                    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov",
                    "dec",
                ];
                let month = MONTHS.iter().position(|m| *m == c[2].to_lowercase())? as u32 + 1;
                let day = c.get(1).or(c.get(3)).and_then(|m| m.as_str().parse().ok());
                calendar_span(c[4].parse().ok()?, month, day)
            })
            .collect();
    }
    if out.is_empty() {
        out = signed_years(text)
            .into_iter()
            .map(|y| (y, y + 1.0))
            .collect();
    }
    out
}

/// One day, or one month when `day` is `None`, as a span of fractional years.
fn calendar_span(year: i32, month: u32, day: Option<u32>) -> Option<(f64, f64)> {
    use chrono::Datelike;
    let days = |y: i32| {
        if chrono::NaiveDate::from_ymd_opt(y, 2, 29).is_some() {
            366.0
        } else {
            365.0
        }
    };
    let at = |d: chrono::NaiveDate| f64::from(d.year()) + f64::from(d.ordinal0()) / days(d.year());
    let start = chrono::NaiveDate::from_ymd_opt(year, month, day.unwrap_or(1))?;
    let end = match day {
        Some(_) => start.succ_opt()?,
        None if month == 12 => chrono::NaiveDate::from_ymd_opt(year + 1, 1, 1)?,
        None => chrono::NaiveDate::from_ymd_opt(year, month + 1, 1)?,
    };
    let end = if end.year() > start.year() {
        f64::from(end.year())
    } else {
        at(end)
    };
    Some((at(start), end))
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
