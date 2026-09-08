// SPDX-License-Identifier: AGPL-3.0-or-later
//! What an atlas CARRIES — the census a navigation row is checked against
//! before the walk runs it.
//!
//! The navigation map (`EPISTEMIC_INDEX.md` §2.2) says what each row seeds on
//! and which edges it follows. Nothing checked that the atlases in scope
//! carry any of it. Measured 2026-09-08 (order epistemic-index-map-conversion):
//! on wikipedia the tension row classified and walked with
//! `seed_kinds_unseen=[Claim, Position] nodes_reached=0` — a row firing into
//! nothing, reported after the fact by the ledger and never refused before
//! it. This module is the refusal.
//!
//! The rule, pre-registered in the order: a row is ADMISSIBLE for a set of
//! graphs iff some graph carries at least one of its seed kinds AND (the row
//! has no walk OR some graph carries at least one of its edge kinds). Two
//! refinements the measurement forced, both named here so they cannot be
//! mistaken for tuning:
//!
//! - An `Entity` seed narrowed by `entity_types` counts as carried only when
//!   one of those types is present. Wikipedia's atoms are all `Entity` with
//!   subtype `article`; the thematic row seeds on `Entity(concept)`, and a
//!   kind-level census would admit it and then the seed filter would reject
//!   every candidate — the same zero, one step later.
//! - A row that seeds on the DECLARED types (`seed.declared`) counts as
//!   carried only when some graph declared types. That is the enumeration
//!   row, and `seed_admits` in `ground.rs` is already inert for an undeclared
//!   corpus; this makes the walk say so before seeding instead of after.
//!
//! The census is a UNION over the graphs in scope, because the rule is
//! existential over them. It is what the walk can observe without a scan:
//! the atom-class store counts its resident records and the CSR's type bytes
//! at open; the wiki-class store counts what it built. `_summary.json`
//! carries the same census on disk (`atom_counts` + `edge_counts`) for a
//! reader that has no graph open — `svrn atlas kind` and the map-check verb.

use std::collections::BTreeMap;

use corpus_engine_vocab::ontology::WalkPolicy;

use super::atoms::AtomType;
use super::edges::EdgeType;
use super::projection::AtomRecord;
use super::provider::AtlasProvider;
use super::summary::AtlasSummary;

/// The kinds an atlas (or a set of atlases) carries, by count.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AtlasInventory {
    /// Atoms per kind.
    pub atoms: BTreeMap<AtomType, u64>,
    /// `Entity` atoms per `entity_type` tag (`concept`, `person`, …), the
    /// tag as [`super::projection::subtype_of`] spells it. Empty for an atlas
    /// with no entities.
    pub entity_types: BTreeMap<String, u64>,
    /// Edges per kind, as the WALKED graph holds them — the CSR for the
    /// atom-class store, so an edge to a chunk reference (which has no seat
    /// there) is not counted as walkable.
    pub edges: BTreeMap<EdgeType, u64>,
    /// Whether some atlas declared types (`shape.types` non-empty) — what the
    /// enumeration row seeds on.
    pub declares_types: bool,
}

impl AtlasInventory {
    /// The union over the graphs in scope — the census the walk consults.
    /// Empty graphs give an empty inventory, under which every filtered row
    /// is inert; that is the truthful answer for a walk with nothing to walk.
    pub fn of(graphs: &[&dyn AtlasProvider]) -> Self {
        let mut out = Self::default();
        for g in graphs {
            out.absorb(g.inventory());
            if g.ontology().is_some_and(|p| p.has_declarations()) {
                out.declares_types = true;
            }
        }
        out
    }

    /// The tally over a store's resident records and its edge census — the
    /// ONE counting loop every provider uses (§10.6). An Entity's subtype is
    /// its `entity_type` tag, which is what [`AtomRecord::subtype`] carries.
    pub fn from_records<'a>(
        atoms: impl IntoIterator<Item = &'a AtomRecord>,
        edges: &BTreeMap<EdgeType, u64>,
    ) -> Self {
        let mut out = Self {
            edges: edges.clone(),
            ..Default::default()
        };
        for r in atoms {
            *out.atoms.entry(r.kind).or_insert(0) += 1;
            if r.kind == AtomType::Entity && !r.subtype.is_empty() {
                *out.entity_types.entry(r.subtype.clone()).or_insert(0) += 1;
            }
        }
        out
    }

    /// The census as `_summary.json` carries it, for a reader with no graph
    /// open. `entity_types` is read off `subtype_counts`, which keys every
    /// kind's subtype in one map; an entity type and a claim kind sharing a
    /// spelling would be conflated here, and no built-in vocabulary has such
    /// a pair.
    pub fn from_summary(s: &AtlasSummary) -> Self {
        Self {
            atoms: s.atom_counts.clone(),
            entity_types: s.subtype_counts.clone(),
            edges: s.edge_counts.clone(),
            declares_types: s.ontology.is_some(),
        }
    }

    /// Fold another census into this one.
    pub fn absorb(&mut self, other: AtlasInventory) {
        for (k, n) in other.atoms {
            *self.atoms.entry(k).or_insert(0) += n;
        }
        for (k, n) in other.entity_types {
            *self.entity_types.entry(k).or_insert(0) += n;
        }
        for (k, n) in other.edges {
            *self.edges.entry(k).or_insert(0) += n;
        }
        self.declares_types |= other.declares_types;
    }

    /// One atom of this kind, at least.
    pub fn carries(&self, kind: AtomType) -> bool {
        self.atoms.get(&kind).is_some_and(|n| *n > 0)
    }

    /// One edge of this kind, at least.
    pub fn carries_edge(&self, kind: EdgeType) -> bool {
        self.edges.get(&kind).is_some_and(|n| *n > 0)
    }

    /// Nothing counted at all — no graph was in scope, or none carried an
    /// atom. Distinguished so a caller can say "no inventory" rather than
    /// "every row inert", which is the same verdict for a different reason.
    pub fn is_empty(&self) -> bool {
        self.atoms.values().all(|n| *n == 0) && !self.declares_types
    }

    /// Can this row fire on these atlases? The pre-registered rule, in one
    /// place (§10.6): the walk and the CLI verbs both ask here.
    pub fn fit(&self, row: &WalkPolicy) -> RowFit {
        let seed = &row.seed;
        let mut inert = RowInert::default();

        // Seeds. An unfiltered seed (no kinds, not declared) admits whatever
        // the pool returns and is never inert on this axis.
        let filtered = !seed.kinds.is_empty() || seed.declared;
        if filtered {
            let mut any = false;
            for k in &seed.kinds {
                let carried = if *k == AtomType::Entity && !seed.entity_types.is_empty() {
                    seed.entity_types.iter().any(|t| {
                        self.entity_types
                            .get(t.as_str_repr())
                            .is_some_and(|n| *n > 0)
                    })
                } else {
                    self.carries(*k)
                };
                if carried {
                    any = true;
                } else {
                    inert.seeds_missing.push(*k);
                }
            }
            if seed.declared {
                if self.declares_types {
                    any = true;
                } else {
                    inert.declared_missing = true;
                }
            }
            if any {
                inert.seeds_missing.clear();
                inert.declared_missing = false;
            }
        }

        // Edges. No walk (enumeration, or the unfiltered row's "every kind")
        // is never inert on this axis.
        if !row.walk.is_empty() && !row.walk.iter().any(|e| self.carries_edge(*e)) {
            inert.edges_missing = row.walk.clone();
        }

        if inert.is_fit() {
            RowFit::Fits
        } else {
            RowFit::Inert(inert)
        }
    }
}

/// The verdict on one row against one inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowFit {
    Fits,
    Inert(RowInert),
}

impl RowFit {
    pub fn fits(&self) -> bool {
        matches!(self, RowFit::Fits)
    }

    /// `fits`, `inert:seeds` or `inert:edges` — the verdict column.
    pub fn verdict(&self) -> &'static str {
        match self {
            RowFit::Fits => "fits",
            RowFit::Inert(i) => i.verdict(),
        }
    }
}

/// Why a row cannot fire: which of its seed kinds and edge kinds nothing in
/// scope carries. At least one axis is non-empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowInert {
    /// Every seed kind the row lists, when none of them is carried. Empty
    /// when the seed axis is fine.
    pub seeds_missing: Vec<AtomType>,
    /// The row seeds on declared types and nothing in scope declared any.
    pub declared_missing: bool,
    /// Every edge kind the row walks, when none of them is carried. Empty
    /// when the edge axis is fine or the row has no walk.
    pub edges_missing: Vec<EdgeType>,
}

impl RowInert {
    fn is_fit(&self) -> bool {
        self.seeds_missing.is_empty() && !self.declared_missing && self.edges_missing.is_empty()
    }

    /// Seeds first: a row with nothing to seed on never reaches its edges.
    pub fn verdict(&self) -> &'static str {
        if !self.seeds_missing.is_empty() || self.declared_missing {
            "inert:seeds"
        } else {
            "inert:edges"
        }
    }

    /// One clause for a sentence: `no Claim or Position atoms`, `no Tension
    /// or OpposesIn edges`, `no declared types`.
    pub fn clause(&self) -> String {
        let mut parts = Vec::new();
        if !self.seeds_missing.is_empty() {
            parts.push(format!(
                "no {} atoms",
                self.seeds_missing
                    .iter()
                    .map(|k| k.label())
                    .collect::<Vec<_>>()
                    .join(" or ")
            ));
        }
        if self.declared_missing {
            parts.push("no declared types".to_string());
        }
        if !self.edges_missing.is_empty() {
            parts.push(format!(
                "no {} edges",
                self.edges_missing
                    .iter()
                    .map(|e| e.label())
                    .collect::<Vec<_>>()
                    .join(" or ")
            ));
        }
        parts.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine_vocab::ontology::{NavigationPolicy, QuestionKind};

    /// Wikipedia as the wiki-class store builds it: every atom an `Entity`
    /// with subtype `article`, every edge an `Involves`, nothing declared.
    fn wikipedia() -> AtlasInventory {
        AtlasInventory {
            atoms: BTreeMap::from([(AtomType::Entity, 1_600_000)]),
            entity_types: BTreeMap::from([("article".to_string(), 1_600_000)]),
            edges: BTreeMap::from([(EdgeType::Involves, 40_000_000)]),
            declares_types: false,
        }
    }

    /// A SEP article as installed: Claims and Configurations, Involves /
    /// Grounds / Tension edges, no Position, no Summary, nothing declared.
    fn sep_article() -> AtlasInventory {
        AtlasInventory {
            atoms: BTreeMap::from([
                (AtomType::Entity, 57),
                (AtomType::Claim, 32),
                (AtomType::Configuration, 3),
                (AtomType::State, 17),
                (AtomType::ArgumentReconstruction, 6),
            ]),
            entity_types: BTreeMap::from([("concept".to_string(), 40), ("person".to_string(), 17)]),
            edges: BTreeMap::from([
                (EdgeType::Involves, 60),
                (EdgeType::Grounds, 12),
                (EdgeType::Tension, 5),
            ]),
            declares_types: false,
        }
    }

    /// THE FAILING INPUT: the wikipedia tension walk of 2026-09-08
    /// (`seed_kinds_unseen=[Claim, Position] nodes_reached=0`). Under the
    /// rule it is inert on seeds — and the verdict names the seeds, not the
    /// edges, because a row with nothing to seed on never reaches an edge.
    #[test]
    fn the_wikipedia_tension_row_is_inert_on_seeds() {
        let inv = wikipedia();
        let map = NavigationPolicy::default();
        let fit = inv.fit(map.walk(QuestionKind::Tension));
        let RowFit::Inert(why) = fit else {
            panic!("the tension row must not fit an Entity-only atlas");
        };
        assert_eq!(why.seeds_missing, vec![AtomType::Claim, AtomType::Position]);
        assert_eq!(
            why.edges_missing,
            vec![EdgeType::Tension, EdgeType::OpposesIn]
        );
        assert_eq!(why.verdict(), "inert:seeds");
        assert_eq!(
            why.clause(),
            "no claim or position atoms; no tension or opposes_in edges"
        );
    }

    /// The whole table against wikipedia, row by row. Lookup is the one row
    /// that fits; the thematic row is refused on the ENTITY TYPE, which a
    /// kind-level census would have admitted.
    #[test]
    fn wikipedia_fits_lookup_and_nothing_else() {
        let inv = wikipedia();
        let map = NavigationPolicy::default();
        assert!(inv.fit(map.walk(QuestionKind::Lookup)).fits());
        assert_eq!(
            inv.fit(map.walk(QuestionKind::Thematic)).verdict(),
            "inert:seeds"
        );
        assert_eq!(
            inv.fit(map.walk(QuestionKind::Trajectory)).verdict(),
            "inert:edges"
        );
        assert_eq!(
            inv.fit(map.walk(QuestionKind::Enumeration)).verdict(),
            "inert:seeds"
        );
        let RowFit::Inert(enumeration) = inv.fit(map.walk(QuestionKind::Enumeration)) else {
            panic!("enumeration seeds on declared types and wikipedia declares none");
        };
        assert!(enumeration.declared_missing);
        assert_eq!(enumeration.clause(), "no declared types");
        // Declare a type and the enumeration row fits — the one bit flips
        // the verdict, so the bit is load-bearing.
        let mut declared = wikipedia();
        declares(&mut declared);
        assert!(declared.fit(map.walk(QuestionKind::Enumeration)).fits());
    }

    fn declares(inv: &mut AtlasInventory) {
        inv.declares_types = true;
    }

    /// The pre-registered tension row fits a SEP article on `Claim` alone:
    /// ONE carried seed kind is enough, and the `Position` gap is the
    /// per-walk `seed_kinds_unseen` observation, not a refusal.
    #[test]
    fn one_carried_seed_kind_admits_the_row() {
        let inv = sep_article();
        let map = NavigationPolicy::default();
        assert!(inv.fit(map.walk(QuestionKind::Tension)).fits());
        assert!(inv.fit(map.walk(QuestionKind::Thematic)).fits());
        assert!(inv.fit(map.walk(QuestionKind::Lookup)).fits());
        // Trajectory walks Transition / Causes and the article has neither.
        assert_eq!(
            inv.fit(map.walk(QuestionKind::Trajectory)).verdict(),
            "inert:edges"
        );
    }

    /// The unfiltered row fits everything, an empty inventory included: no
    /// seed filter and no edge filter means nothing to be inert on. It is the
    /// row every refusal falls to, so it must never be refused itself.
    #[test]
    fn the_unfiltered_row_always_fits() {
        assert!(AtlasInventory::default()
            .fit(&WalkPolicy::unfiltered())
            .fits());
        assert!(wikipedia().fit(&WalkPolicy::unfiltered()).fits());
    }

    /// The union is existential: a scope holding wikipedia AND a SEP
    /// article carries Claims, so the tension row fits the scope.
    #[test]
    fn the_union_admits_what_any_graph_carries() {
        let mut inv = wikipedia();
        inv.absorb(sep_article());
        assert!(inv
            .fit(NavigationPolicy::default().walk(QuestionKind::Tension))
            .fits());
        assert_eq!(inv.atoms[&AtomType::Entity], 1_600_057);
        assert!(!inv.is_empty());
        assert!(AtlasInventory::default().is_empty());
    }
}
