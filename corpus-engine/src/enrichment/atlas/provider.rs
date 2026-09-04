// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`AtlasProvider`] — one store provider for the walk (operator ruling,
//! 2026-09-04: "one store PROVIDER everywhere").
//!
//! # What this trait is for
//!
//! The walk (`ground`) asks an atlas eight questions and
//! nothing else: what is this atom, what passages does this atom cite, what
//! edges leave it, what edges enter it, where is your ANN seed table, what
//! did you declare, what is your id, and where do your chunks live. Until now
//! the only thing that could answer them was
//! [`AtlasGraph`] — the v2 store, `atoms.lance` resident plus the `edges.csr`
//! mmap — so "an atlas the walk can read" and "a v2 store on disk" were the
//! same sentence.
//!
//! They are not the same thing. Wikipedia's 1.6M atoms live in `edges.lance`
//! plus a 2.4 GB SQLite graph (`ATLAS_STORAGE_V2`'s burn-down list,
//! `EPISTEMIC_INDEX.md` §3), and folding that into the v2 store is a large
//! migration. The walk does not need the migration; it needs an answer to nine
//! questions. So the questions become a trait, [`AtlasGraph`] becomes its
//! first implementor, and a second store implements it without touching the
//! walk.
//!
//! # Why exactly these eight
//!
//! ARCH §5.1 (trait surface) and §19 (the inventory outranks the plan): the
//! set is what `ground.rs` and `atom_verbatim_excerpt` actually call, read off
//! the call sites, not what an atlas could conceivably offer. [`AtlasGraph`]
//! has more than thirty public methods — `call_chain`, `resolve_symbol_seed`,
//! `atom_count`, the loaders — and none of them is here, because the walk does
//! not call them and a method in a trait is a promise every future implementor
//! must keep.
//!
//! `atoms_of_kind` was in the first draft of this trait and is NOT here. No
//! caller used it: the walk observes absent seed kinds over its seed POOL
//! rather than scanning per kind, to keep an O(n) scan off the hot path, and
//! the wiki-class backend does not want it either (wikipedia is one kind —
//! 1.67M `Entity` articles — so a kind filter there is the identity function).
//! A trait member that no implementor needs and no caller calls is inventory
//! (§19), and it came out before the second implementor had to keep the
//! promise.
//!
//! # The views are constructible, or this is not a seam
//!
//! [`AtomView`] and [`EvidenceRef`] are newtypes over `&AtomRecord` and
//! `&ChunkRef`, and five of these eight members return them. Their tuple
//! fields are private, so until [`AtomView::new`] / [`EvidenceRef::new`]
//! existed a second backend could not build a single one of those five
//! returns — the trait would have been implementable only by the module that
//! defines the views. A seam that only its author can cross is not a seam;
//! the constructors are part of the contract, not a convenience.
//!
//! # What is deliberately NOT here
//!
//! No `load`, no `open`, no writer. Construction is each store's own business
//! and differs completely between a Lance preload and a SQLite handle; a
//! constructor in the trait would force one store's lifecycle onto the other.
//! The walk receives already-open providers.

use std::sync::Arc;

use crate::enrichment::ontology::{OntologyPolicies, TypeIndex};

use super::ann_store::AnnSeedTable;
use super::context::{AtlasGraph, AtomView, EdgeView, EvidenceRef};
use super::evidence_site::EvidenceSite;

/// What the grounding walk needs from a store of ideas.
///
/// See the module doc for why the surface is this narrow. Implement it for a
/// store, and `ground` walks it — the walk knows nothing
/// about how atoms are held.
///
/// `Send + Sync` is not decoration. The walk is async (the ANN query awaits)
/// and runs inside the retrieval pipeline's boxed `Send` future, so a
/// provider held across that await must be both; without the bounds
/// `sovereign-core` fails to compile with "future cannot be sent between
/// threads safely" at the pipeline's step, several files from the cause.
/// Every store that can be shared by a daemon already satisfies them.
pub trait AtlasProvider: Send + Sync {
    /// This atlas's own id — an EXTRACTION address (`sep-freewill`), never a
    /// corpus to search. Ask [`Self::site`] for that.
    fn atlas_corpus_id(&self) -> &str;

    /// Where the chunks this atlas cites actually live, and whether a title
    /// filter applies. The ONE answer to "which corpus do I search"; see
    /// [`super::evidence_site`] for the defect that makes this a type rather
    /// than a convention.
    fn site(&self) -> &EvidenceSite;

    /// One atom by id, or `None` when this store does not hold it.
    fn atom(&self, atom_id: &str) -> Option<AtomView<'_>>;

    /// The passages one atom cites. Empty means the atom can never become a
    /// citation, which is a fact the walk's ledger reports rather than hides.
    fn atom_evidence(&self, atom_id: &str) -> Vec<EvidenceRef<'_>>;

    /// Edges leaving this atom, carrying the closed
    /// [`EdgeType`](super::edges::EdgeType).
    fn edges_from(&self, atom_id: &str) -> Vec<EdgeView<'_>>;

    /// Edges entering this atom. The walk follows both directions: a
    /// `Grounds` edge is as informative read backwards as forwards.
    fn edges_to(&self, atom_id: &str) -> Vec<EdgeView<'_>>;

    /// The persistent ANN seed table, when this store has been backfilled.
    /// `None` is the store saying it cannot seed by vector, which the walk
    /// names as a degradation rather than treating as an empty result.
    fn ann_seed_table(&self) -> Option<&Arc<AnnSeedTable>>;

    /// What this corpus DECLARED — `Some` only when it declared types, so the
    /// `Option` has already answered `has_declarations()` for every consumer.
    fn ontology(&self) -> Option<&OntologyPolicies>;

    // ── provided ────────────────────────────────────────────────────────
    //
    // Derived from the nine above and identical for every store, so an
    // implementor cannot get them subtly different (ARCH §10.6).

    /// Whether a vector seed is possible at all.
    fn has_ann_seed_table(&self) -> bool {
        self.ann_seed_table().is_some()
    }

    /// The display label for this atlas's site — the article for a per-article
    /// atlas, the corpus id for a self-hosted one. Display and grouping only;
    /// never a corpus to search.
    fn article_slug(&self) -> &str {
        self.site().label()
    }

    /// Is `subtype` the declared type `target`, or a `specializes` descendant
    /// of it? Always `false` for a corpus that declared nothing, which is what
    /// keeps every declared-type code path inert on SEP, Wikipedia and Enron
    /// (the I5 gate).
    ///
    /// The `specializes` chain is walked by [`TypeIndex::is_a`], the ONE place
    /// it is walked; this is the provider-side accessor, not a second
    /// implementation.
    fn is_subtype_of(&self, subtype: &str, target: &str) -> bool {
        match self.ontology() {
            Some(p) => TypeIndex::from_policies(p).is_a(subtype, target),
            None => false,
        }
    }
}

/// The first implementor: the v2 store (`atoms.lance` resident + the
/// `edges.csr` mmap).
///
/// Every method forwards to the inherent one of the same name. The inherent
/// methods stay — dozens of call sites outside the walk use them on a concrete
/// `AtlasGraph`, and making them trait-only would force a `use` on all of them
/// for no gain. What the trait adds is that the WALK no longer names this
/// type.
impl AtlasProvider for AtlasGraph {
    fn atlas_corpus_id(&self) -> &str {
        &self.atlas_corpus_id
    }

    fn site(&self) -> &EvidenceSite {
        &self.site
    }

    fn atom(&self, atom_id: &str) -> Option<AtomView<'_>> {
        AtlasGraph::atom(self, atom_id)
    }

    fn atom_evidence(&self, atom_id: &str) -> Vec<EvidenceRef<'_>> {
        AtlasGraph::atom_evidence(self, atom_id)
    }

    fn edges_from(&self, atom_id: &str) -> Vec<EdgeView<'_>> {
        AtlasGraph::edges_from(self, atom_id)
    }

    fn edges_to(&self, atom_id: &str) -> Vec<EdgeView<'_>> {
        AtlasGraph::edges_to(self, atom_id)
    }

    fn ann_seed_table(&self) -> Option<&Arc<AnnSeedTable>> {
        AtlasGraph::ann_seed_table(self)
    }

    fn ontology(&self) -> Option<&OntologyPolicies> {
        AtlasGraph::ontology(self)
    }
}

#[cfg(test)]
mod tests {
    use super::super::atoms::{AtomType, ChunkRef};
    use super::super::projection::AtomRecord;
    use super::*;

    /// A backend that is NOT `AtlasGraph` and does not live in `context.rs` —
    /// the second implementor the trait exists for, standing in for
    /// ei-7c's wiki-class store.
    struct Elsewhere {
        site: EvidenceSite,
        record: AtomRecord,
        ontology: Option<OntologyPolicies>,
    }

    impl Elsewhere {
        fn new(ontology: Option<OntologyPolicies>) -> Self {
            Self {
                site: EvidenceSite::derive("elsewhere"),
                record: AtomRecord {
                    id: "atom-1".into(),
                    kind: AtomType::Entity,
                    name: "Offa".into(),
                    label: String::new(),
                    content: String::new(),
                    subtype: "person".into(),
                    description: "King of Mercia".into(),
                    excerpt: String::new(),
                    confidence: 0.0,
                    salience: 1.0,
                    aliases: Vec::new(),
                    participants: Vec::new(),
                    evidence: vec![ChunkRef::new("sec_0001", Some("a silver penny".into()))],
                    payload: Vec::new(),
                },
                ontology,
            }
        }
    }

    impl AtlasProvider for Elsewhere {
        fn atlas_corpus_id(&self) -> &str {
            "elsewhere"
        }
        fn site(&self) -> &EvidenceSite {
            &self.site
        }
        fn atom(&self, atom_id: &str) -> Option<AtomView<'_>> {
            (atom_id == self.record.id).then(|| AtomView::new(&self.record))
        }
        fn atom_evidence(&self, atom_id: &str) -> Vec<EvidenceRef<'_>> {
            if atom_id != self.record.id {
                return Vec::new();
            }
            self.record.evidence.iter().map(EvidenceRef::new).collect()
        }
        fn edges_from(&self, _: &str) -> Vec<EdgeView<'_>> {
            Vec::new()
        }
        fn edges_to(&self, _: &str) -> Vec<EdgeView<'_>> {
            Vec::new()
        }
        fn ann_seed_table(&self) -> Option<&Arc<AnnSeedTable>> {
            None
        }
        fn ontology(&self) -> Option<&OntologyPolicies> {
            self.ontology.as_ref()
        }
    }

    /// THE SEAM: a provider defined outside `context.rs` builds and returns
    /// both view types. Four of the eight members return an `AtomView` or an
    /// `EvidenceRef`, and their tuple fields are private — so before
    /// `AtomView::new` / `EvidenceRef::new` existed, this impl block could not
    /// be written at all and the trait was implementable only by its author's
    /// own module.
    ///
    /// Failing input: make either constructor private again — this file stops
    /// compiling, which is the loudest failure available and the right one.
    #[test]
    fn a_backend_outside_this_module_can_build_the_views() {
        let p = Elsewhere::new(None);
        let atom = p.atom("atom-1").expect("the record is there");
        assert_eq!(atom.id(), "atom-1");
        assert_eq!(atom.name(), "Offa");
        assert_eq!(atom.kind(), AtomType::Entity);
        assert_eq!(atom.subtype(), "person");

        let ev = p.atom_evidence("atom-1");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].chunk_id(), "sec_0001");
        assert_eq!(ev[0].passage_preview(), "a silver penny");
        // …and an id it does not hold is absent, not an empty view.
        assert!(p.atom("atom-2").is_none());
        assert!(p.atom_evidence("atom-2").is_empty());
    }

    /// The trait is usable behind `dyn` — which is the whole point, since the
    /// walk holds a heterogeneous slice of providers. Failing input: add a
    /// generic method or a `Self: Sized` return to the trait.
    #[test]
    fn the_provider_is_object_safe() {
        fn assert_dyn(_: Option<&dyn AtlasProvider>) {}
        assert_dyn(None);
    }

    /// The provided `is_subtype_of`, BOTH ways: a provider that declared
    /// `sceatta specializes coin` says so, and one that declared nothing is
    /// inert — with no `has_declarations()` check at either call site.
    ///
    /// Two-sided deliberately (§18.1, "a zero count is not a positive
    /// control"): the `false` half alone is satisfied by a method that always
    /// returns `false`, which is the exact regression that matters on the
    /// declared side.
    ///
    /// Failing inputs: return `true` from `is_subtype_of` when `ontology()`
    /// is `None`; or stop consulting `TypeIndex::is_a` so the `specializes`
    /// chain is not walked.
    #[test]
    fn is_subtype_of_answers_both_ways_without_a_declaration_check() {
        // POSITIVE control: a declared `sceatta specializes coin`.
        let declared: OntologyPolicies = serde_json::from_value(serde_json::json!({
            "shape": { "types": [
                { "name": "coin", "kind": "entity" },
                { "name": "sceatta", "kind": "entity", "specializes": "coin" }
            ] }
        }))
        .expect("the declaration parses");
        let p = Elsewhere::new(Some(declared));
        assert!(
            p.is_subtype_of("sceatta", "coin"),
            "the specializes chain must be walked through TypeIndex::is_a"
        );
        assert!(!p.is_subtype_of("coin", "sceatta"), "and only downward");

        // NEGATIVE control: the same two questions, undeclared.
        let p = Elsewhere::new(None);
        assert!(!p.is_subtype_of("sceatta", "coin"));
        // …and a store with no seed table says so, rather than being asked
        // for one and returning an empty answer.
        assert!(!p.has_ann_seed_table());
        // The provided site label is derived, not re-implemented per backend.
        assert_eq!(p.article_slug(), "elsewhere");
    }
}
