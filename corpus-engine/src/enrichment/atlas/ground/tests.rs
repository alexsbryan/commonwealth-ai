// SPDX-License-Identifier: AGPL-3.0-or-later
//! The walk's tests. Split out of `ground.rs` by order ei-5c so the
//! 1,815-line source becomes three files that each answer one question
//! (ARCH §3.1). Nothing here changed in the move; `super::*` still resolves
//! to the walk, which is what these tests exercise.

use super::*;
use corpus_engine_vocab::taxonomy::EntityType;

// Named here rather than through `super::*`: the walk stopped importing them
// when `select.rs` took the classification half, and a test module that leans
// on its parent's private imports is a test module that breaks on the next
// split.
use crate::atlas_traversal::question_kind::KindSource;
use crate::types::EmbedFn;
use corpus_engine_vocab::ontology::{NavigationPolicy, QuestionKind, WalkPolicy};

use crate::enrichment::atlas::ann_store::AnnSeedTable;
use crate::enrichment::atlas::context::{AtlasEntry, AtomView, EdgeView, EvidenceRef};
use crate::enrichment::atlas::projection::AtomRecord;
use corpus_engine_vocab::atoms::ChunkRef;
use corpus_engine_vocab::edges::EdgeProvenance;

// ── The displacement fixture (§18.1) ────────────────────────────────
//
// One rollup atom whose evidence is five chunks, and five ordinary
// atoms with one chunk each. The rollup matches the question better
// than any single leaf does — which is the NORMAL case for a rollup,
// not a contrived one: a paraphrase of a whole region is written to
// be about the region.
//
// Everything below is shared by the two tests that follow. They
// differ in ONE field — the rollup atom's `kind` — and that field is
// the whole hold-out. Keeping both directions as tests is deliberate:
// a guard whose failing input is not itself run is a guard nobody has
// watched fail (ARCH §18.1), and "I disabled it once locally" is not
// evidence anybody can re-check.

const ROLLUP_CHUNKS: [&str; 5] = ["c1", "c2", "c3", "c4", "c5"];
const LEAF_CHUNKS: [&str; 5] = ["c6", "c7", "c8", "c9", "c10"];

struct FixtureAtlas {
    site: EvidenceSite,
    atoms: std::collections::HashMap<String, AtomRecord>,
    edges: Vec<(String, String, EdgeType)>,
}

fn record(id: &str, kind: AtomType, content: &str, chunks: &[&str]) -> AtomRecord {
    AtomRecord {
        id: id.to_string(),
        kind,
        name: String::new(),
        label: String::new(),
        content: content.to_string(),
        subtype: String::new(),
        description: String::new(),
        excerpt: String::new(),
        confidence: 0.0,
        salience: 0.0,
        aliases: Vec::new(),
        participants: Vec::new(),
        evidence: chunks
            .iter()
            .map(|c| ChunkRef::new(*c, Some(format!("preview of {c}"))))
            .collect(),
        payload: Vec::new(),
    }
}

/// `rollup_kind` is the ONLY variable. `Summary` is the shipped
/// behaviour; any leaf-grain kind reproduces the pre-ei-7a mechanism.
fn fixture(rollup_kind: AtomType) -> FixtureAtlas {
    let mut atoms = std::collections::HashMap::new();
    atoms.insert(
        "rollup".to_string(),
        record(
            "rollup",
            rollup_kind,
            "a paraphrase of the whole region",
            &ROLLUP_CHUNKS,
        ),
    );
    let mut edges = Vec::new();
    for (i, chunk) in LEAF_CHUNKS.iter().enumerate() {
        let id = format!("leaf{i}");
        atoms.insert(
            id.clone(),
            record(&id, AtomType::Claim, "an ordinary claim", &[chunk]),
        );
        // The rollup cites the leaves it summarises. This is what makes
        // the hazard structural rather than incidental: EvidenceFor is
        // exactly the edge the port introduces, and it fans out over the
        // whole subtree by construction.
        edges.push(("rollup".to_string(), id, EdgeType::EvidenceFor));
    }
    FixtureAtlas {
        site: EvidenceSite::derive("fixture"),
        atoms,
        edges,
    }
}

impl AtlasProvider for FixtureAtlas {
    // Named, not defaulted, exactly as the trait requires: a fixture that
    // borrowed "atom-class" would let a test assert against a store it is
    // not, which is the silent-identical-arms failure the required method
    // exists to make visible.
    fn provider_class(&self) -> &'static str {
        "fixture-class"
    }

    fn atlas_corpus_id(&self) -> &str {
        "fixture"
    }
    fn site(&self) -> &EvidenceSite {
        &self.site
    }
    fn atom(&self, atom_id: &str) -> Option<AtomView<'_>> {
        self.atoms.get(atom_id).map(AtomView::new)
    }
    fn atom_evidence(&self, atom_id: &str) -> Vec<EvidenceRef<'_>> {
        self.atoms
            .get(atom_id)
            .map(|a| a.evidence.iter().map(EvidenceRef::new).collect())
            .unwrap_or_default()
    }
    fn edges_from(&self, atom_id: &str) -> Vec<EdgeView<'_>> {
        self.edges
            .iter()
            .filter(|(s, _, _)| s == atom_id)
            .map(|(s, t, k)| EdgeView {
                source: s.as_str(),
                target: t.as_str(),
                edge_type: *k,
                confidence: 1.0,
                provenance: EdgeProvenance::Derived,
            })
            .collect()
    }
    fn edges_to(&self, atom_id: &str) -> Vec<EdgeView<'_>> {
        self.edges
            .iter()
            .filter(|(_, t, _)| t == atom_id)
            .map(|(s, t, k)| EdgeView {
                source: s.as_str(),
                target: t.as_str(),
                edge_type: *k,
                confidence: 1.0,
                provenance: EdgeProvenance::Derived,
            })
            .collect()
    }
    fn ann_seed_table(&self) -> Option<&std::sync::Arc<AnnSeedTable>> {
        None
    }
    fn ontology(&self) -> Option<&OntologyPolicies> {
        None
    }
}

/// The bag that seeds the walk by name match. The rollup's embedding
/// IS the question (cosine 1.0); the leaves are further away. A
/// rollup outscoring its own leaves is the ordinary case.
fn fixture_bag() -> AtlasContext {
    let mut entries = vec![AtlasEntry {
        atom_id: "rollup".to_string(),
        canonical_name: "rollup".to_string(),
        embed_text: "rollup".to_string(),
        embedding: vec![1.0, 0.0],
    }];
    for i in 0..LEAF_CHUNKS.len() {
        entries.push(AtlasEntry {
            atom_id: format!("leaf{i}"),
            canonical_name: format!("leaf{i}"),
            embed_text: format!("leaf{i}"),
            embedding: vec![0.5, 0.866],
        });
    }
    AtlasContext {
        atlas_corpus_id: "fixture".to_string(),
        entries,
        top_k: 32,
    }
}

async fn walk_fixture(rollup_kind: AtomType) -> Grounding {
    let atlas = fixture(rollup_kind);
    let bag = fixture_bag();
    // Names every atom so seeding is deterministic and does not depend
    // on an ANN table the fixture deliberately does not have.
    let question = "rollup leaf0 leaf1 leaf2 leaf3 leaf4";
    ground(
        question,
        &[1.0, 0.0],
        &[&bag],
        &[&atlas as &dyn AtlasProvider],
        &WalkSelection::unfiltered(KindSource::Abstained, None, PolicySource::PreRegistered),
        12,
    )
    .await
}

fn requested_chunks(g: &Grounding) -> Vec<String> {
    g.requests
        .iter()
        .map(|r| r.selector.as_str().to_string())
        .collect()
}

/// THE FAILING INPUT, kept runnable. With the rollup carrying a
/// LEAF-grain kind — which is exactly what a RAPTOR summary was
/// before `AtomType::Summary` existed — its weight lands on all five
/// of the chunks it covers, and those chunks outrank every leaf. This
/// is the −14pt SEP source-coverage mechanism (2026-06-08) reproduced
/// deterministically, with no model and no corpus.
///
/// If this test ever goes green-by-accident (no displacement), the
/// sibling test below is proving nothing and both need re-deriving.
#[tokio::test]
async fn a_leaf_grain_rollup_displaces_the_leaf_chunks_it_covers() {
    let g = walk_fixture(AtomType::Claim).await;
    let chunks = requested_chunks(&g);
    let top: Vec<&String> = chunks.iter().take(ROLLUP_CHUNKS.len()).collect();
    for c in ROLLUP_CHUNKS {
        assert!(
            top.iter().any(|t| t.as_str() == c),
            "rollup chunk {c} should have crowded the top; got {chunks:?}"
        );
    }
    assert!(
        g.summaries.is_empty(),
        "a leaf-grain atom is not carried as a summary"
    );
    assert_eq!(g.ledger.summary_seeds, 0);
}

/// THE GUARD. Same fixture, same scores, same edges — the rollup is
/// `Summary` grain. R2 keeps its weight off every leaf request, so the
/// five leaf chunks are the whole request list and the rollup's text
/// leaves by the other door (R3) instead.
#[tokio::test]
async fn a_summary_seed_cannot_displace_a_leaf_chunk() {
    let g = walk_fixture(AtomType::Summary).await;
    let chunks = requested_chunks(&g);

    for c in ROLLUP_CHUNKS {
        assert!(
            !chunks.iter().any(|r| r == c),
            "summary-covered chunk {c} entered the requests; the hold-out did not fire \
                 ({chunks:?})"
        );
    }
    for c in LEAF_CHUNKS {
        assert!(
            chunks.iter().any(|r| r == c),
            "leaf chunk {c} was displaced ({chunks:?})"
        );
    }

    // R3: the text is carried, not dropped — the capability survives
    // the hold-out. Losing it would be the other way to pass this test
    // and is not a fix.
    assert_eq!(
        g.summaries.len(),
        1,
        "the summary is carried for late append"
    );
    assert_eq!(g.summaries[0].atom_id, "rollup");
    assert!(g.summaries[0].text.contains("paraphrase"));

    // R1 + the ledger: the hold-out is VISIBLE, not silent.
    assert_eq!(g.ledger.summary_seeds, 1);
    assert!(
        g.ledger.summary_expansions_suppressed >= 1,
        "R1 never fired: {:?}",
        g.ledger
    );
    assert_eq!(g.ledger.summaries_appended, 1);
}

// ── The SEED-RACE fixture (ei-5c, §18.1) ────────────────────────────
//
// The displacement above is about SCORING: a rollup's weight landing on
// the leaf chunks it covers. R1/R2 closed that, and the two tests above
// watch both directions of it.
//
// This is the OTHER displacement, and it is the one that kept the walk
// from being sole. Seeds come out of ONE score-ordered pool capped at
// `max_seeds`. A Summary is written to be about a whole region, so it
// outscores any single leaf on a thematic question — which means that
// making Summary reachable (ei-7a) made it a COMPETITOR for every seed
// slot. Measured on the SEP subset A/B: OFF 47/66 twice, ON 40/66 and
// 39/66, −7.5/66 on a HARD lane, with Summary taking about 1.1% of slots
// against ~21k entity and argument seeds.
//
// No score, kind or edge below is contrived to make the point: the
// summaries score 1.0 and the leaves 0.6 because that is what
// `ground`'s own name-match seeding assigns them (`cosine(...).max(0.6)`),
// and there are more summaries than the row's quota because a real
// article's RAPTOR tree has dozens of nodes.

/// A thematic-row fixture: `summaries` Summary atoms and `leaves` concept
/// Entity atoms, every one of them named in the question so seeding is
/// deterministic without an ANN table. Each atom cites one chunk of its
/// own, so "which seeds won" is readable straight off the request list.
fn seed_race_fixture(summaries: usize, leaves: usize) -> (FixtureAtlas, AtlasContext) {
    let mut atoms = std::collections::HashMap::new();
    let mut entries = Vec::new();
    for i in 0..summaries {
        let id = format!("sum{i}");
        let mut rec = record(&id, AtomType::Summary, "a paraphrase", &[&format!("s{i}")]);
        rec.name = id.clone();
        atoms.insert(id.clone(), rec);
        entries.push(AtlasEntry {
            atom_id: id.clone(),
            canonical_name: id,
            embed_text: "summary".to_string(),
            // Cosine 1.0 against the question — the rollup's normal case.
            embedding: vec![1.0, 0.0],
        });
    }
    for i in 0..leaves {
        let id = format!("leaf{i}");
        let mut rec = record(
            &id,
            AtomType::Entity,
            "an ordinary idea",
            &[&format!("l{i}")],
        );
        rec.name = id.clone();
        rec.subtype = EntityType::Concept.as_str_repr().to_string();
        atoms.insert(id.clone(), rec);
        entries.push(AtlasEntry {
            atom_id: id.clone(),
            canonical_name: id,
            embed_text: "leaf".to_string(),
            // Cosine 0.5, floored to 0.6 by the name-match rule — every
            // leaf loses the race to every summary, every time.
            embedding: vec![0.5, 0.866],
        });
    }
    let question = entries
        .iter()
        .map(|e| e.canonical_name.clone())
        .collect::<Vec<_>>()
        .join(" ");
    let bag = AtlasContext {
        atlas_corpus_id: "fixture".to_string(),
        entries,
        top_k: 32,
    };
    let _ = question;
    (
        FixtureAtlas {
            site: EvidenceSite::derive("fixture"),
            atoms,
            edges: Vec::new(),
        },
        bag,
    )
}

async fn walk_thematic(policy: &NavigationPolicy, summaries: usize, leaves: usize) -> Grounding {
    let (atlas, bag) = seed_race_fixture(summaries, leaves);
    let question = bag
        .entries
        .iter()
        .map(|e| e.canonical_name.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    ground(
        &question,
        &[1.0, 0.0],
        &[&bag],
        &[&atlas as &dyn AtlasProvider],
        &WalkSelection::named(QuestionKind::Thematic, policy, PolicySource::PreRegistered),
        12,
    )
    .await
}

/// THE FAILING INPUT, kept runnable: the same fixture under a map whose
/// thematic row declares NO quota — the pre-ei-5c table exactly. Twenty
/// summaries outscore every leaf, take all twelve seed slots, and the
/// walk emits ZERO leaf evidence requests.
///
/// This is what "reachable but never at a leaf's expense" could not be
/// said before [`SeedPolicy::budgets`] existed, and the reason the
/// retrieval-time injector had to stay.
#[tokio::test]
async fn without_a_quota_summaries_take_every_seed_slot() {
    let mut policy = NavigationPolicy::default();
    policy.thematic.seed.budgets.clear();
    let g = walk_thematic(&policy, 20, 5).await;

    assert_eq!(g.ledger.seeds, 12, "the whole pool went somewhere");
    assert_eq!(
        g.ledger.summary_seeds, 12,
        "every slot went to a summary; ledger {:?}",
        g.ledger
    );
    assert!(
        g.requests.is_empty(),
        "no leaf seeded, so there is no leaf evidence to request: {:?}",
        requested_chunks(&g)
    );
    // The drops here are the SHARED pool overflowing, not a per-kind
    // quota: `max_seeds` is the default quota and every candidate is in
    // it. That the counter cannot tell the two apart is the point of the
    // guard below — only the row's `budgets` can.
    assert_eq!(
        g.ledger.dropped_seed_budget, 13,
        "25 candidates, 12 slots, one shared pool"
    );
    assert_eq!(g.ledger.summaries_appended, SUMMARY_SEED_BUDGET as usize);
}

/// THE GUARD. Same fixture, the pre-registered map: `Summary` draws from
/// its own quota of [`SUMMARY_SEED_BUDGET`] and the leaves keep the full
/// `max_seeds` pool, so all five leaves seed and every leaf chunk is
/// requested — while the summaries are still REACHED and carried out.
///
/// Both halves matter. Dropping Summary from the row would also make the
/// leaf assertion pass, and would be the capability loss, not the fix.
#[tokio::test]
async fn a_quota_keeps_summaries_reachable_without_costing_a_leaf_seed() {
    let g = walk_thematic(&NavigationPolicy::default(), 20, 5).await;

    assert_eq!(
        g.ledger.summary_seeds, SUMMARY_SEED_BUDGET as usize,
        "the quota is the cap, not a suggestion; ledger {:?}",
        g.ledger
    );
    assert_eq!(
        g.ledger.seeds,
        SUMMARY_SEED_BUDGET as usize + 5,
        "the quota is drawn from its OWN pool — the five leaves are on top \
             of it, not squeezed out by it"
    );
    assert!(
        g.ledger.dropped_seed_budget >= 12,
        "the twelve surplus summaries are refused BY THE QUOTA and said so \
             in the ledger, not silently truncated: {:?}",
        g.ledger
    );

    let chunks = requested_chunks(&g);
    for i in 0..5 {
        let c = format!("l{i}");
        assert!(
            chunks.contains(&c),
            "leaf chunk {c} was displaced by a summary seed ({chunks:?})"
        );
    }
    assert_eq!(
        g.summaries.len(),
        SUMMARY_SEED_BUDGET as usize,
        "the summaries are still carried out — R3's cap is the same quota"
    );
}

/// The fetched pool must be able to hold the quota AND the unbudgeted slots.
///
/// Failing input, and it is the reason the function exists: size the pool on
/// `max_seeds` alone (drop the `quota` term) and the pre-registered thematic
/// row fetches 48 where it needs 80. On a summary-dense atlas the fetched
/// window fills with the kind the quota then refuses, and the leaves the quota
/// was protecting starve INSIDE the pool — upstream of the filter, so
/// `dropped_seed_budget` counts the refusals and nothing counts the leaves
/// that were never fetched. The displacement, one step earlier and invisible.
#[test]
fn the_fetched_pool_holds_the_quota_on_top_of_the_shared_slots() {
    let n = NavigationPolicy::default();
    let thematic = n.walk(QuestionKind::Thematic);
    assert_eq!(
        seed_pool_size(thematic, 12),
        (12 + SUMMARY_SEED_BUDGET as usize) * 4,
        "the pool must fetch for max_seeds PLUS every declared quota"
    );

    // A row with no quota is unchanged — the pre-ei-5c size exactly.
    let tension = n.walk(QuestionKind::Tension);
    assert_eq!(seed_pool_size(tension, 12), 12 * 4);

    // A map may declare any `u32`, and the property that matters is that the
    // result cannot WRAP: a wrapped pool size is a tiny number, i.e. a silent
    // under-fetch, which is the failure mode this whole function is about.
    // Asserted as "at least the quota" rather than as `usize::MAX`, because
    // saturation is not reached on a 64-bit usize and pinning the exact
    // product would be pinning the target's word size.
    let mut wild = WalkPolicy::thematic();
    wild.seed.budgets.insert(AtomType::Summary, u32::MAX);
    assert!(seed_pool_size(&wild, 12) >= u32::MAX as usize);
}

/// R3's cap and the row's quota are ONE number. A map that declares a
/// smaller quota gets fewer seeds AND fewer appended summaries from the
/// same edit; they cannot drift, which is what `SUMMARY_APPEND_CAP` (the
/// constant this replaced) allowed.
#[tokio::test]
async fn the_appended_summary_count_follows_the_declared_quota() {
    let mut policy = NavigationPolicy::default();
    policy.thematic.seed.budgets.insert(AtomType::Summary, 2);
    let g = walk_thematic(&policy, 20, 5).await;
    assert_eq!(g.ledger.summary_seeds, 2);
    assert_eq!(g.summaries.len(), 2);
    assert_eq!(g.ledger.summaries_appended, 2);
}

/// The chunk → atlas id derivation, in both shapes, and its agreement
/// with `EvidenceSite`'s reading in the other direction. Failing input:
/// drop the self-hosted candidate, or emit the child for a titleless
/// chunk.
#[test]
fn candidate_atlas_ids_covers_both_layouts_and_agrees_with_evidence_site() {
    let ids = candidate_atlas_ids("sep", Some("freewill"));
    assert_eq!(ids, vec!["sep".to_string(), "sep-freewill".to_string()]);
    // The inverse holds: the child id reads back to the parent corpus.
    assert_eq!(
        EvidenceSite::derive("sep-freewill").chunk_corpus().as_str(),
        "sep"
    );

    // A chunk with no title has exactly one candidate — its own corpus.
    assert_eq!(
        candidate_atlas_ids("wikipedia", None),
        vec!["wikipedia".to_string()]
    );
    assert_eq!(
        candidate_atlas_ids("wikipedia", Some("   ")),
        vec!["wikipedia".to_string()]
    );
    // …and a chunk titled after its own corpus yields ONE candidate, not
    // a `bk-1-bk-1` that addresses nothing. This is the literary shape,
    // not a corner case: every chunk of `brothers-karamazov-book-1` is
    // titled with its corpus id.
    assert_eq!(
        candidate_atlas_ids("bk-1", Some("bk-1")),
        vec!["bk-1".to_string()]
    );
}

/// The unfiltered row admits everything and does NOT over-fetch — the
/// path `apply_atlas_grounding` has always taken. Failing input: make
/// `seed_filter_is_active` true for the unfiltered row.
#[test]
fn the_unfiltered_row_is_the_status_quo_ante() {
    let w = WalkPolicy::unfiltered();
    assert!(!seed_filter_is_active(&w));
    assert!(w.walk.is_empty(), "no edge filter");
    assert_eq!(w.hops, 2);
    assert_eq!(w.budget, corpus_engine_vocab::ontology::DEFAULT_BUDGET);
}

/// Every classified row DOES filter, or the map is not driving anything.
#[test]
fn every_pre_registered_row_filters_its_seeds() {
    let p = NavigationPolicy::default();
    for (kind, w) in p.rows() {
        assert!(
            seed_filter_is_active(w),
            "{} declares no seed filter",
            kind.as_str()
        );
    }
}

/// The enumeration row seeds on declared types only — so it can never
/// fire on a corpus that declared nothing, which is the I5 gate.
#[test]
fn the_enumeration_row_is_inert_without_a_declaration() {
    let p = NavigationPolicy::default();
    let w = p.walk(QuestionKind::Enumeration);
    assert!(w.seed.declared && w.seed.kinds.is_empty());
    assert_eq!(w.hops, 0, "enumeration lists its seeds, it does not walk");
}

/// The thematic row narrows `Entity` to concepts and leaves
/// `Configuration` unnarrowed — the two arms of `seed_admits`.
#[test]
fn an_entity_type_narrows_only_the_entity_seed() {
    let p = NavigationPolicy::default();
    let w = p.walk(QuestionKind::Thematic);
    assert_eq!(w.seed.entity_types, vec![EntityType::Concept]);
    assert!(w.seed.kinds.contains(&AtomType::Configuration));
    // `Configuration` carries whatever subtype it likes and still seeds;
    // `Entity` must be a concept.
    assert_eq!(EntityType::Concept.as_str_repr(), "concept");
}

/// Every degradation renders a sentence a reader can act on — none is an
/// empty string or a bare enum name.
#[test]
fn every_degradation_says_something_actionable() {
    let all = [
        Degradation::NoSeedTable,
        Degradation::NoAtomBag,
        Degradation::Unclassified(KindSource::Abstained),
        Degradation::SeedKindsUnseen(vec![AtomType::Configuration]),
        Degradation::NoEvidenceAnchors,
        Degradation::MapTruncated { reached: 900 },
    ];
    for d in all {
        let s = d.sentence();
        assert!(s.len() > 20, "{d:?} -> {s:?}");
        assert!(s.chars().next().unwrap().is_lowercase() || s.starts_with("the"));
    }
    assert!(Degradation::SeedKindsUnseen(vec![AtomType::Configuration])
        .sentence()
        .contains("configuration"));
}

/// No graphs is an EMPTY grounding that still names why — never a bare
/// vec the caller reads as "nothing was relevant".
#[tokio::test]
async fn a_walk_with_no_graphs_says_why_it_is_empty() {
    let g = ground(
        "what is this about",
        &[0.1, 0.2],
        &[],
        &[],
        &WalkSelection::unfiltered(KindSource::Abstained, None, PolicySource::PreRegistered),
        12,
    )
    .await;
    assert!(g.requests.is_empty());
    assert!(g
        .degradations
        .contains(&Degradation::Unclassified(KindSource::Abstained)));
}

/// An empty embedding cannot seed, and says so rather than returning an
/// unexplained empty list.
#[tokio::test]
async fn an_empty_embedding_cannot_seed() {
    let g = ground(
        "anything",
        &[],
        &[],
        &[],
        &WalkSelection::named(
            QuestionKind::Thematic,
            &NavigationPolicy::default(),
            PolicySource::PreRegistered,
        ),
        12,
    )
    .await;
    assert!(g.requests.is_empty());
    assert_eq!(g.ledger.seeds, 0);
}

/// With no classifier available the walk runs the unfiltered row and the
/// source says which — it does not silently pick a kind.
#[tokio::test]
async fn no_embedder_means_the_unfiltered_row_named_as_such() {
    let sel = select_walk(
        &[0.1, 0.2],
        &NavigationPolicy::default(),
        PolicySource::PreRegistered,
        None,
    )
    .await;
    assert_eq!(sel.kind, QuestionKind::Thematic);
    assert_eq!(sel.walk, WalkPolicy::unfiltered());
    assert_eq!(sel.kind_source, KindSource::ClassifierUnavailable);
    assert!(sel.kind_source.is_degradation());
    assert!(sel.kind_score.is_none());
    // …and it SAYS so, in one line a reader can act on.
    assert!(sel.describe().contains("classifier-unavailable"));
}

/// A map that carries no exemplars anywhere yields `NoClassifier`, which
/// is a DIFFERENT fact from an unreachable embedder.
#[tokio::test]
async fn a_map_with_no_exemplars_is_distinguished_from_a_dead_embedder() {
    let mut policy = NavigationPolicy::default();
    for row in [
        &mut policy.thematic,
        &mut policy.trajectory,
        &mut policy.tension,
        &mut policy.enumeration,
        &mut policy.lookup,
    ] {
        row.exemplars.clear();
    }
    let embed: EmbedFn = std::sync::Arc::new(|_: &str| {
        Box::pin(async { Ok(vec![1.0_f32, 0.0]) })
            as std::pin::Pin<Box<dyn std::future::Future<Output = crate::Result<Vec<f32>>> + Send>>
    });
    let sel = select_walk(
        &[1.0, 0.0],
        &policy,
        PolicySource::PreRegistered,
        Some(&embed),
    )
    .await;
    assert_eq!(sel.kind_source, KindSource::NoClassifier);
    assert_eq!(sel.walk, WalkPolicy::unfiltered());
}

/// No declared navigation anywhere means the pre-registered table, and
/// the source says so rather than looking like a corpus's own choice.
#[test]
fn an_undeclared_scope_uses_the_pre_registered_table() {
    let (policy, src) = navigation_policy_for(&[]);
    assert_eq!(policy, NavigationPolicy::default());
    assert_eq!(src, PolicySource::PreRegistered);
    assert_eq!(src.label(), "pre-registered defaults");
}
