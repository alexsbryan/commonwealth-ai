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
    /// Which STORE CLASS is serving this corpus — `"atom-class"` or
    /// `"wiki-class"`.
    ///
    /// Required, with no default, deliberately (principle 10: structural, not
    /// remembered). An A/B whose arms differ only in their store has to show
    /// that each arm was served by the store it names; "atom-class" appearing
    /// where "wiki-class" was expected is the silent-identical-arms failure,
    /// and it is invisible unless something says which one answered. A default
    /// here would let a new backend inherit another class's label, which is
    /// worse than no label at all.
    fn provider_class(&self) -> &'static str;

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
    fn provider_class(&self) -> &'static str {
        "atom-class"
    }

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

/// Open whatever the grounding walk can read for one atlas — the ONE place
/// that decides which store class a corpus is (ARCH §10.6).
///
/// # The order is the whole logic
///
/// An atom store is tried FIRST, because a corpus that has one is atom-class
/// by definition. Only when [`AtlasGraph::load_from_disk`] refuses — which it
/// does for wikipedia, by design, since wikipedia carries no `atoms.lance` —
/// does the wiki-class store get its turn, and only if
/// [`wikipedia_graph_present`](crate::wikipedia_graph_present) says there is
/// one. That predicate is the ONE answer to "does this corpus have a link
/// graph", so this does not re-derive the question.
///
/// # Why it is a free function and not a trait method
///
/// [`AtlasProvider`] deliberately carries no `open` (see the module doc):
/// construction differs completely between a Lance preload and a columnar
/// mmap, and a constructor in the trait would force one store's lifecycle
/// onto the other. But the CHOICE between them is not a lifecycle detail —
/// it is one decision that every host must make identically, and before this
/// existed each host made it for itself. `sovereign-tools`'
/// `AtlasContextManager::walk_provider` and `corpus-mcp`'s host both call
/// this now, so a corpus cannot be atom-class to the daemon and unreadable to
/// the MCP host.
///
/// # Seeding is class-agnostic
///
/// Both branches attach the ANN seed table through
/// [`open_ann_seed_table`](super::context::open_ann_seed_table), because the
/// table is a property of the atlas DIRECTORY rather than of the backend. A
/// wiki-class store therefore gets vector seeding the moment its table is
/// built, with no second wiring.
///
/// # Errors
///
/// `Err` NAMES what was missing rather than collapsing to a bare `None`
/// (ARCH §18.3): a corpus with no store at all and a corpus whose wiki store
/// is present but unusable (a v1 store with no `atom_id`/`chunk_id`, which
/// cannot be cited and so must not be walked) are different facts, and the
/// caller's degradation text says which. Callers fall back to their
/// bag-of-atoms path; this function never substitutes one class for the other
/// silently.
///
/// MUST run on the caller's long-lived async runtime — see
/// [`open_ann_seed_table`](super::context::open_ann_seed_table) for why the
/// held `lancedb::Table` cannot come from a throwaway one. Sync callers use
/// [`open_walk_provider_blocking`].
pub async fn open_walk_provider(
    indexes_dir: &std::path::Path,
    atlas_corpus_id: &str,
) -> Result<Arc<dyn AtlasProvider>, String> {
    let atlas_dir = indexes_dir.join(atlas_corpus_id).join(super::ATLAS_DIRNAME);
    let started = std::time::Instant::now();

    let atom_err = match AtlasGraph::load_from_disk(atlas_corpus_id, &atlas_dir) {
        Ok(g) => {
            let g = super::context::open_and_attach_ann_seed_table(atlas_corpus_id, &atlas_dir, g)
                .await;
            tracing::info!(
                corpus = atlas_corpus_id,
                backend = "atom-class",
                seed_table = g.has_ann_seed_table(),
                load_ms = started.elapsed().as_millis(),
                "walk provider: opened"
            );
            return Ok(Arc::new(g) as Arc<dyn AtlasProvider>);
        }
        Err(e) => e,
    };

    if !crate::wikipedia_graph_present(indexes_dir, atlas_corpus_id) {
        return Err(format!(
            "no store the walk can read for `{atlas_corpus_id}`: {atom_err}; and no wiki-class \
             store (articles.lance + edges.lance) under {}",
            atlas_dir.display()
        ));
    }

    match crate::WikiAtlasProvider::open(&atlas_dir, atlas_corpus_id).await {
        Ok(p) => {
            let ann = super::context::open_ann_seed_table(atlas_corpus_id, &atlas_dir).await;
            let p = match ann {
                Some(a) => p.with_ann_seed_table(a),
                None => p,
            };
            tracing::info!(
                corpus = atlas_corpus_id,
                backend = "wiki-class",
                atoms = p.atom_count(),
                edges = p.edge_count(),
                seed_table = p.has_ann_seed_table(),
                load_ms = started.elapsed().as_millis(),
                "walk provider: opened"
            );
            Ok(Arc::new(p) as Arc<dyn AtlasProvider>)
        }
        Err(e) => Err(format!(
            "wiki-class store present but unusable for `{atlas_corpus_id}`: {e}"
        )),
    }
}

/// [`open_walk_provider`] for a sync caller, bridged through the atlas
/// module's ONE async-from-sync bridge. Lifecycle time only — corpus load,
/// never the hot query path.
pub fn open_walk_provider_blocking(
    indexes_dir: &std::path::Path,
    atlas_corpus_id: &str,
) -> Result<Arc<dyn AtlasProvider>, String> {
    super::store::run_blocking(open_walk_provider(indexes_dir, atlas_corpus_id))
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
        fn provider_class(&self) -> &'static str {
            "test-double"
        }

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
