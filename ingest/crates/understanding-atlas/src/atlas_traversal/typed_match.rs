// SPDX-License-Identifier: AGPL-3.0-or-later
//! The typed query's per-atom judgements, split from [`super::typed`] (the
//! query's shape and its answer) and [`super::typed_check`] (the query checked
//! against the vocabulary, and how values read). Four rules live here and
//! nowhere else:
//!
//! - **A named far end** is an atom whose canonical name or an alias equals
//!   the name under [`fold`] (case + diacritics) — never a substring, so the
//!   name "Sardes" does not land on a mint called "Sardes Serpent workshop",
//!   the defect `context/views.rs`'s generic-head guard has with "coins".
//! - **A time** reads as a signed-year interval (B.C. negative) and **a
//!   quantity** as a number interval; `lt` holds when the interval ends before
//!   the bound, `gt` when it starts after it, `eq` when it spans it.
//! - **A link** is a Relation atom of the declared relation type (or a
//!   subtype) naming both atoms as participants, in either order, or a ref
//!   attribute holding the far end's atom id.
//! - **An absence is not a no.** An atom that does not carry a filtered
//!   attribute, or carries it in a form that does not read, is set aside as
//!   UNJUDGED, counted per type and attribute, and reported in the answer's
//!   notes (ARCH §18.3) — under `negate` too, where "no value" would otherwise
//!   pass as "not before 300 B.C.".
//! - **Two attributes compare as their values do with a literal**: each side
//!   reads as its interval, `lt` holds when the filtered side ends before the
//!   other starts, `gt` the reverse, `eq` when they overlap; either side
//!   unset or unreadable is unjudged, on the atom and attribute that lacked it.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{Map, Value};

use crate::enrichment::atlas::atoms::{AtomEnvelope, Claim, Entity, Event, Relation};
use crate::enrichment::atlas::fold;
use crate::enrichment::ontology::{OntologyPolicies, TypeIndex};

use super::engine::AtlasView;
use super::typed::FilterOp;
use super::typed_check::{
    numbers, time_spans, Checked, CheckedFilter, CheckedRelation, Link, Operand, Side, ValueKind,
    SUBJECT,
};

/// An atom a typed query can return: the two kinds that carry both a declared
/// subtype and an `attributes` map.
#[derive(Clone, Copy, Debug)]
pub(super) enum Atom<'a> {
    Entity(&'a Entity),
    /// A declared event type's record (a novel's incident, a filing's hearing).
    Event(&'a Event),
    Claim(&'a Claim),
}

impl<'a> Atom<'a> {
    pub(super) fn id(self) -> &'a str {
        match self {
            Atom::Entity(e) => e.id.as_str(),
            Atom::Event(e) => e.id.as_str(),
            Atom::Claim(c) => c.id.as_str(),
        }
    }

    /// What a row calls the atom: an entity's canonical name, an event's
    /// description, a claim's content.
    pub(super) fn name(self) -> &'a str {
        match self {
            Atom::Entity(e) => &e.canonical_name,
            Atom::Event(e) => &e.description,
            Atom::Claim(c) => &c.content,
        }
    }

    pub(super) fn attributes(self) -> &'a Map<String, Value> {
        match self {
            Atom::Entity(e) => &e.attributes,
            Atom::Event(e) => &e.attributes,
            Atom::Claim(c) => &c.attributes,
        }
    }

    /// The atom ids (or written names) this atom holds under `attr`: its ref
    /// attribute's values, else a claim's `subject`, the record it is about.
    pub(super) fn held(self, attr: &str) -> Vec<&'a str> {
        if let Some(v) = self.attributes().get(attr) {
            return strings(v);
        }
        match self {
            Atom::Claim(c) if attr == SUBJECT => c.subject.iter().map(|s| s.as_str()).collect(),
            _ => Vec::new(),
        }
    }

    /// The declared subtype; a claim with no `claim_kind` has none (`""`,
    /// which no declaration names, so it is no type's instance).
    fn subtype(self) -> &'a str {
        match self {
            Atom::Entity(e) => e.entity_type.as_str_repr(),
            Atom::Event(e) => e.event_type.as_str_repr(),
            Atom::Claim(c) => c.claim_kind.as_deref().unwrap_or(""),
        }
    }

    /// Row order: salience descending (claims carry none and sort after
    /// entities), then atom id, so the order is total and stable.
    pub(super) fn rank(a: Atom<'_>, b: Atom<'_>) -> Ordering {
        let s = |x: Atom<'_>| match x {
            Atom::Entity(e) => Some(e.salience),
            Atom::Event(_) | Atom::Claim(_) => None,
        };
        match (s(a), s(b)) {
            (Some(x), Some(y)) => y.partial_cmp(&x).unwrap_or(Ordering::Equal),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
        .then_with(|| a.id().cmp(b.id()))
    }

    /// Evidence chunk ids, through the one evidence accessor
    /// ([`AtomEnvelope::evidence`]), which knows the field each kind keeps
    /// them in (an Entity's is its first appearance), each as the reader
    /// cites it (`ChunkRef::cited_id`: the installed chunk when placed). The
    /// clone is the price of asking about a borrowed atom; it runs once per
    /// returned row.
    pub(super) fn chunk_ids(self) -> Vec<String> {
        let envelope = match self {
            Atom::Entity(e) => AtomEnvelope::Entity(e.clone()),
            Atom::Event(e) => AtomEnvelope::Event(e.clone()),
            Atom::Claim(c) => AtomEnvelope::Claim(c.clone()),
        };
        envelope
            .evidence()
            .into_iter()
            .map(|c| c.cited_id())
            .collect()
    }
}

/// What carried a satisfied link: the Relation atom, or the atom whose ref
/// attribute named the far end. Its evidence is the row's citation for it.
#[derive(Clone, Copy, Debug)]
pub(super) enum Via<'a> {
    Relation(&'a Relation),
    Ref(Atom<'a>),
}

impl Via<'_> {
    pub(super) fn chunk_ids(&self) -> Vec<String> {
        match self {
            Via::Relation(r) => AtomEnvelope::Relation((*r).clone())
                .evidence()
                .into_iter()
                .map(|c| c.cited_id())
                .collect(),
            Via::Ref(a) => a.chunk_ids(),
        }
    }
}

/// The query's target while its relations are judged: what a `{"target": ..}`
/// operand inside a `where` reads.
#[derive(Clone, Copy)]
pub(super) struct Near<'a, 'q> {
    atom: Atom<'a>,
    ty: &'q str,
}

/// The atom and attribute a filter could not read, and so could not judge.
struct Unread {
    ty: String,
    attr: String,
    atom: String,
}

impl Unread {
    fn of(ty: &str, attr: &str, atom: Atom<'_>) -> Self {
        Self {
            ty: ty.to_string(),
            attr: attr.to_string(),
            atom: atom.id().to_string(),
        }
    }
}

/// The atlas, indexed for judging, plus the running count of atoms set aside
/// as unjudged.
pub(super) struct Judge<'a> {
    pub(super) index: TypeIndex<'a>,
    pub(super) policies: &'a OntologyPolicies,
    all: Vec<Atom<'a>>,
    by_id: HashMap<&'a str, Atom<'a>>,
    /// Folded canonical name + aliases, per atom id.
    names: HashMap<&'a str, Vec<String>>,
    /// Relation atoms per participant id.
    relations_of: HashMap<&'a str, Vec<&'a Relation>>,
    /// (ref attribute, atom id it holds) -> the atoms holding it.
    referrers: HashMap<(&'a str, &'a str), Vec<Atom<'a>>>,
    /// (type, attribute) -> ids of atoms that could not be judged on it.
    unjudged: RefCell<BTreeMap<(String, String), BTreeSet<String>>>,
}

impl<'a> Judge<'a> {
    pub(super) fn new(atlas: AtlasView<'a>, policies: &'a OntologyPolicies) -> Self {
        let all: Vec<Atom<'a>> = atlas
            .entities
            .iter()
            .map(Atom::Entity)
            .chain(atlas.events.iter().map(Atom::Event))
            .chain(atlas.claims.iter().map(Atom::Claim))
            .collect();
        let by_id = all.iter().map(|a| (a.id(), *a)).collect();
        let names = atlas
            .entities
            .iter()
            .map(|e| {
                let forms = std::iter::once(&e.canonical_name)
                    .chain(&e.aliases)
                    .map(|n| fold(n))
                    .collect();
                (e.id.as_str(), forms)
            })
            .collect();
        let mut relations_of: HashMap<&'a str, Vec<&'a Relation>> = HashMap::new();
        for r in atlas.relations {
            let mut seen = BTreeSet::new();
            for p in &r.participants {
                if seen.insert(p.as_str()) {
                    relations_of.entry(p.as_str()).or_default().push(r);
                }
            }
        }
        let mut referrers: HashMap<(&'a str, &'a str), Vec<Atom<'a>>> = HashMap::new();
        for a in &all {
            for (k, v) in a.attributes() {
                for held in strings(v) {
                    referrers.entry((k.as_str(), held)).or_default().push(*a);
                }
            }
            if !a.attributes().contains_key(SUBJECT) {
                for held in a.held(SUBJECT) {
                    referrers.entry((SUBJECT, held)).or_default().push(*a);
                }
            }
        }
        Self {
            index: TypeIndex::from_policies(policies),
            policies,
            all,
            by_id,
            names,
            relations_of,
            referrers,
            unjudged: RefCell::new(BTreeMap::new()),
        }
    }

    pub(super) fn of_type(&self, t: &str) -> Vec<Atom<'a>> {
        self.all
            .iter()
            .copied()
            .filter(|a| self.is_a(*a, t))
            .collect()
    }

    fn is_a(&self, atom: Atom<'_>, t: &str) -> bool {
        self.index.is_a(atom.subtype(), t)
    }

    /// Is `atom` called `folded` — canonical name or alias, both folded? A
    /// claim is named by its content.
    fn named(&self, atom: Atom<'_>, folded: &str) -> bool {
        match self.names.get(atom.id()) {
            Some(forms) => forms.iter().any(|f| f == folded),
            None => fold(atom.name()) == folded,
        }
    }

    /// Does `atom` meet every filter and relation constraint? `Some(via)` with
    /// what carried each satisfied link; `None` when any fails or cannot be
    /// judged.
    pub(super) fn qualifies(&self, atom: Atom<'a>, c: &Checked<'_>) -> Option<Vec<Via<'a>>> {
        let near = Near { atom, ty: c.target };
        self.meets(atom, c.target, &c.filters, &c.relations, near)
    }

    fn meets(
        &self,
        atom: Atom<'a>,
        t: &str,
        filters: &[CheckedFilter<'_>],
        relations: &[CheckedRelation<'_>],
        near: Near<'a, '_>,
    ) -> Option<Vec<Via<'a>>> {
        for f in filters {
            let judged = match f.operand {
                Operand::Attr { side, attribute } => {
                    self.compare_holds(atom, t, f, side, attribute, near)
                }
                _ => self
                    .filter_holds(atom, t, f)
                    .ok_or_else(|| Unread::of(t, f.attribute, atom)),
            };
            match judged {
                Ok(true) => {}
                Ok(false) => {
                    tracing::debug!(
                        atom = atom.id(),
                        attribute = f.attribute,
                        "atlas traversal: typed filter fails"
                    );
                    return None;
                }
                Err(u) => {
                    tracing::debug!(
                        atom = u.atom.as_str(),
                        attribute = u.attr.as_str(),
                        filtered = atom.id(),
                        "atlas traversal: typed filter cannot judge an unset or unreadable value"
                    );
                    self.unjudged
                        .borrow_mut()
                        .entry((u.ty, u.attr))
                        .or_default()
                        .insert(u.atom);
                    return None;
                }
            }
        }
        let mut via = Vec::new();
        for r in relations {
            let found = self.witnesses(atom, r, near);
            if found.is_empty() != r.negate {
                tracing::debug!(
                    atom = atom.id(),
                    relation = r.relation,
                    other_type = r.other_type,
                    other_name = r.other_name,
                    negate = r.negate,
                    "atlas traversal: typed relation constraint fails"
                );
                return None;
            }
            via.extend(found.into_iter().flatten());
        }
        Some(via)
    }

    /// `Some(holds)` after `negate`, or `None` when the atom carries no
    /// readable value — which `negate` does not turn into a yes.
    fn filter_holds(&self, atom: Atom<'_>, t: &str, f: &CheckedFilter<'_>) -> Option<bool> {
        let raw = match (&f.kind, &f.operand) {
            (ValueKind::Name, Operand::Text(needle)) => match f.op {
                FilterOp::Eq => self.named(atom, needle),
                _ => self.names_of(atom).iter().any(|n| word_start(n, needle)),
            },
            (ValueKind::Text, Operand::Text(needle)) => {
                let held: Vec<String> = texts(atom.attributes().get(f.attribute)?);
                if held.is_empty() {
                    return None;
                }
                held.iter().any(|h| match f.op {
                    FilterOp::Eq => fold(h) == *needle,
                    _ => word_start(&fold(h), needle),
                })
            }
            // Spans: `lt` wholly before the bound, `gt` wholly after it, `eq` overlapping it.
            (ValueKind::Time, Operand::Span(start, end)) => {
                let (lo, hi) = self.interval(atom, f.attribute, ValueKind::Time)?;
                match f.op {
                    FilterOp::Lt => hi <= *start,
                    FilterOp::Gt => lo >= *end,
                    _ => lo < *end && *start < hi,
                }
            }
            (ValueKind::Quantity, Operand::Num(v)) => {
                let (lo, hi) = self.interval(atom, f.attribute, ValueKind::Quantity)?;
                match f.op {
                    FilterOp::Lt => hi < *v,
                    FilterOp::Gt => lo > *v,
                    _ => lo <= *v && *v <= hi,
                }
            }
            _ => unreachable!("check pairs each value kind with its operand ({t})"),
        };
        Some(raw != f.negate)
    }

    /// A filter whose operand is another attribute, after `negate`; `Err`
    /// names the side that held no readable value, which `negate` does not
    /// turn into a yes. Text compares folded, by `eq` only (check refuses
    /// `contains`); times and quantities as intervals, as with a literal.
    fn compare_holds(
        &self,
        atom: Atom<'_>,
        t: &str,
        f: &CheckedFilter<'_>,
        side: Side,
        other_attr: &str,
        near: Near<'_, '_>,
    ) -> Result<bool, Unread> {
        let (other, other_t) = match side {
            Side::Same => (atom, t),
            Side::Target => (near.atom, near.ty),
        };
        let mine = || Unread::of(t, f.attribute, atom);
        let theirs = || Unread::of(other_t, other_attr, other);
        let raw = match f.kind {
            ValueKind::Text => {
                let folded = |a: Atom<'_>, attr: &str| {
                    let held: Vec<String> = texts(a.attributes().get(attr)?)
                        .iter()
                        .map(|h| fold(h))
                        .collect();
                    (!held.is_empty()).then_some(held)
                };
                let left = folded(atom, f.attribute).ok_or_else(mine)?;
                let right = folded(other, other_attr).ok_or_else(theirs)?;
                left.iter().any(|l| right.contains(l))
            }
            ValueKind::Time | ValueKind::Quantity => {
                let (lo, hi) = self.interval(atom, f.attribute, f.kind).ok_or_else(mine)?;
                let (olo, ohi) = self
                    .interval(other, other_attr, f.kind)
                    .ok_or_else(theirs)?;
                // Half-open time spans touch without overlapping; quantity
                // intervals are closed.
                match (f.kind, f.op) {
                    (ValueKind::Time, FilterOp::Lt) => hi <= olo,
                    (ValueKind::Time, FilterOp::Gt) => lo >= ohi,
                    (ValueKind::Time, _) => lo < ohi && olo < hi,
                    (_, FilterOp::Lt) => hi < olo,
                    (_, FilterOp::Gt) => lo > ohi,
                    _ => lo <= ohi && olo <= hi,
                }
            }
            ValueKind::Name => unreachable!("check refuses an attribute operand on `name` ({t})"),
        };
        tracing::trace!(
            atom = atom.id(),
            attribute = f.attribute,
            compared_with = other_attr,
            other = other.id(),
            holds = raw,
            "atlas traversal: typed filter compared two attributes"
        );
        Ok(raw != f.negate)
    }

    fn names_of(&self, atom: Atom<'_>) -> Vec<String> {
        self.names
            .get(atom.id())
            .cloned()
            .unwrap_or_else(|| vec![fold(atom.name())])
    }

    /// A time or quantity attribute as an interval, or `None` when unset or
    /// unreadable: a time's from its earliest span's start to its latest span's
    /// end (`time_spans`, half-open), a quantity's from its least to its greatest
    /// number.
    pub(super) fn interval(
        &self,
        atom: Atom<'_>,
        attr: &str,
        kind: ValueKind,
    ) -> Option<(f64, f64)> {
        let spans: Vec<(f64, f64)> = match (atom.attributes().get(attr)?, kind) {
            (Value::Number(n), ValueKind::Time) => {
                n.as_f64().map(|y| (y, y + 1.0)).into_iter().collect()
            }
            (Value::Number(n), _) => n.as_f64().map(|x| (x, x)).into_iter().collect(),
            (Value::String(s), ValueKind::Time) => time_spans(s),
            (Value::String(s), _) => numbers(s).into_iter().map(|x| (x, x)).collect(),
            _ => Vec::new(),
        };
        let lo = spans.iter().map(|s| s.0).reduce(f64::min)?;
        let hi = spans.iter().map(|s| s.1).reduce(f64::max)?;
        Some((lo, hi))
    }

    /// A quantity attribute as the ONE number a sum adds, read by the one
    /// quantity reader ([`numbers`]); anything else is why it cannot be added.
    /// A minus before the number is refused rather than read: [`numbers`]
    /// reads `-50` as 50 (it reads `8.5-8.6` as a span), so summing it as
    /// -50 would be a second reading of the value the filters judge.
    pub(super) fn one_number(&self, atom: Atom<'_>, attr: &str) -> Result<f64, Unsummed> {
        let held: Vec<f64> = match atom.attributes().get(attr) {
            None | Some(Value::Null) => return Err(Unsummed::Unset),
            Some(Value::Number(n)) => n.as_f64().into_iter().collect(),
            Some(Value::String(s)) if s.trim_start().starts_with(['-', '\u{2212}']) => {
                return Err(Unsummed::Signed)
            }
            Some(Value::String(s)) => numbers(s),
            Some(other) => texts(other).iter().flat_map(|t| numbers(t)).collect(),
        };
        match held.as_slice() {
            [x] => Ok(*x),
            [] => Err(Unsummed::NoNumber),
            _ => Err(Unsummed::MoreThanOne),
        }
    }

    /// The canonical name (or description, or content) of the atom `id`
    /// names, when it names one.
    pub(super) fn record_name(&self, id: &str) -> Option<&'a str> {
        self.by_id.get(id).map(|a| a.name())
    }

    /// Every far end that satisfies `r` for `atom` (ignoring `negate`), each
    /// with what carried its link — the link itself and, through a `where`,
    /// the links that qualified the far end.
    fn witnesses(
        &self,
        atom: Atom<'a>,
        r: &CheckedRelation<'_>,
        near: Near<'a, '_>,
    ) -> Vec<Vec<Via<'a>>> {
        let folded = r.other_name.map(fold);
        let mut out = Vec::new();
        for (far, via) in self.reach(atom, r.link) {
            match far {
                Far::Atom(y) => {
                    if !self.is_a(y, r.other_type)
                        || folded.as_deref().is_some_and(|n| !self.named(y, n))
                    {
                        continue;
                    }
                    let mut chain = vec![via];
                    if let Some(w) = &r.within {
                        match self.meets(y, r.other_type, &w.filters, &w.relations, near) {
                            Some(inner) => chain.extend(inner),
                            None => continue,
                        }
                    }
                    out.push(chain);
                }
                // A ref holding a name no atom carries: the record still says
                // it, so it answers an unnamed constraint or one naming that
                // same text — never one that needs the far end's own
                // attributes or relations.
                Far::Text(s) => {
                    if r.within.is_none() && folded.as_deref().is_none_or(|n| fold(s) == n) {
                        tracing::debug!(
                            atom = atom.id(),
                            relation = r.relation,
                            held = s,
                            "atlas traversal: ref holds an unresolved name"
                        );
                        out.push(vec![via]);
                    }
                }
            }
        }
        out
    }

    /// Everything `atom` reaches by `link`, with what carries each link.
    fn reach(&self, atom: Atom<'a>, link: Link<'_>) -> Vec<(Far<'a>, Via<'a>)> {
        let id = atom.id();
        match link {
            Link::Relation(rel) => self
                .relations_of
                .get(id)
                .into_iter()
                .flatten()
                .filter(|r| self.index.is_a(r.relation_type.as_str_repr(), rel))
                .flat_map(|r| {
                    r.participants
                        .iter()
                        .filter(|p| p.as_str() != id)
                        .filter_map(|p| self.by_id.get(p.as_str()))
                        .map(move |y| (Far::Atom(*y), Via::Relation(r)))
                })
                .collect(),
            Link::RefOut(attr) => atom
                .held(attr)
                .into_iter()
                .map(|held| match self.by_id.get(held) {
                    Some(y) => (Far::Atom(*y), Via::Ref(atom)),
                    None => (Far::Text(held), Via::Ref(atom)),
                })
                .collect(),
            Link::RefIn(attr) => self
                .referrers
                .get(&(attr, id))
                .into_iter()
                .flatten()
                .map(|y| (Far::Atom(*y), Via::Ref(*y)))
                .collect(),
        }
    }

    /// The distinct atoms of `other` linked to `atom` by any of `links` and
    /// inside every scope the query's relations put on `other` (a non-negated
    /// constraint on that type with a `where`, by its name and `where`), each
    /// with what carried the link.
    pub(super) fn linked_in_scope(
        &self,
        atom: Atom<'a>,
        other: &str,
        links: &[Link<'_>],
        c: &Checked<'_>,
    ) -> Vec<(Atom<'a>, Vec<Via<'a>>)> {
        let scopes: Vec<&CheckedRelation<'_>> = c
            .relations
            .iter()
            .filter(|r| r.other_type == other && r.within.is_some() && !r.negate)
            .collect();
        let near = Near { atom, ty: c.target };
        let mut out: Vec<(Atom<'a>, Vec<Via<'a>>)> = Vec::new();
        for link in links {
            for (far, via) in self.reach(atom, *link) {
                let Far::Atom(y) = far else { continue };
                if !self.is_a(y, other) || out.iter().any(|(z, _)| z.id() == y.id()) {
                    continue;
                }
                let mut chain = vec![via];
                let mut inside = true;
                for s in &scopes {
                    let named = s.other_name.map(fold);
                    let w = s.within.as_ref().expect("filtered on within");
                    match (
                        named.as_deref().is_none_or(|n| self.named(y, n)),
                        self.meets(y, other, &w.filters, &w.relations, near),
                    ) {
                        (true, Some(inner)) => chain.extend(inner),
                        _ => inside = false,
                    }
                }
                if inside {
                    out.push((y, chain));
                }
            }
        }
        out
    }

    /// A note for every named far end no atom of its type carries — the
    /// constraint then holds for nothing (or, negated, for everything), and
    /// the reader must be told that is why.
    pub(super) fn unresolved_names(&self, c: &Checked<'_>) -> Vec<String> {
        let mut notes = Vec::new();
        let inner = c
            .relations
            .iter()
            .filter_map(|r| r.within.as_ref())
            .flat_map(|w| &w.relations);
        for r in c.relations.iter().chain(inner) {
            let Some(name) = r.other_name else { continue };
            let folded = fold(name);
            let known = self
                .of_type(r.other_type)
                .into_iter()
                .any(|y| self.named(y, &folded));
            if !known {
                tracing::debug!(
                    name,
                    other_type = r.other_type,
                    "atlas traversal: named far end resolves to no atom"
                );
                notes.push(format!(
                    "'{name}' names no {} atom in this atlas; the constraint{} is judged on that.",
                    r.other_type,
                    if r.negate { " (negated)" } else { "" }
                ));
            }
        }
        notes
    }

    /// One note per (type, attribute) that set atoms aside as unjudged.
    pub(super) fn unjudged_notes(&self) -> Vec<String> {
        self.unjudged
            .borrow()
            .iter()
            .map(|((t, a), ids)| {
                format!(
                    "{} {t} atom(s) set aside: {a} unset or unreadable.",
                    ids.len()
                )
            })
            .collect()
    }
}

/// Why a sum could not add an atom's value: it is reported, never zeroed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unsummed {
    Unset,
    NoNumber,
    MoreThanOne,
    Signed,
}

impl Unsummed {
    pub(super) fn reason(self) -> &'static str {
        match self {
            Unsummed::Unset => "unset",
            Unsummed::NoNumber => "no number",
            Unsummed::MoreThanOne => "more than one number",
            Unsummed::Signed => "a sign the quantity reader does not read",
        }
    }
}

/// A far end a link reaches: an atom, or the text of a ref that names no atom.
enum Far<'a> {
    Atom(Atom<'a>),
    Text(&'a str),
}

/// The non-empty strings an attribute value holds (a string, or an array of
/// them).
fn strings(v: &Value) -> Vec<&str> {
    match v {
        Value::String(s) if !s.trim().is_empty() => vec![s.as_str()],
        Value::Array(xs) => xs.iter().flat_map(strings).collect(),
        _ => Vec::new(),
    }
}

/// An attribute value as text forms: strings as held, numbers and booleans
/// spelled out; empty when unset.
fn texts(v: &Value) -> Vec<String> {
    match v {
        Value::Number(n) => vec![n.to_string()],
        Value::Bool(b) => vec![b.to_string()],
        other => strings(other).into_iter().map(str::to_string).collect(),
    }
}

/// Does `needle` occur in `hay` starting at a word boundary? Both folded.
/// "Egypt" finds "Egypt (vicinity of Demanhur)" and "Egyptian", not "gypt".
fn word_start(hay: &str, needle: &str) -> bool {
    !needle.is_empty()
        && hay.match_indices(needle).any(|(i, _)| {
            hay[..i]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric())
        })
}
