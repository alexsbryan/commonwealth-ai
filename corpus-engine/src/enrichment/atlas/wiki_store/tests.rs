//! Tests for [`super`] — carved out of `wiki_store.rs` unchanged (ARCH §3.1:
//! the file crossed the 1200-line ceiling). Mechanical move only: no
//! assertion, fixture or name differs from the inline module it replaces.

use super::*;
use arrow_array::Array;
use futures::TryStreamExt;
use lancedb::query::ExecutableQuery;

fn art(title: &str, pov: i64) -> WikiArticleRow {
    WikiArticleRow {
        atom_id: wiki_atom_id(title, "wiki-test"),
        title: title.into(),
        wikidata_qid: format!("Q-{title}"),
        revision_id: 100,
        in_scope: true,
        pov_total: pov,
        citation_total: 5,
        is_contested: pov > 0,
        chunk_id: format!("chunk-{title}"),
    }
}

fn edge(src: &str, tgt: &str, rel: &str, sect: &str, occ: i64, tgt_in: bool) -> WikiEdgeRow {
    WikiEdgeRow {
        source_title: src.into(),
        target_title: tgt.into(),
        relationship_type: rel.into(),
        link_text: tgt.to_lowercase(), // anchor text; the title for this fixture
        occurrence_count: occ,
        source_section_path: sect.into(),
        target_in_scope: tgt_in,
    }
}

async fn read_articles(atlas_dir: &Path) -> Vec<WikiArticleRow> {
    let db = lancedb::connect(atlas_dir.to_str().unwrap())
        .execute()
        .await
        .unwrap();
    let tbl = db.open_table(ARTICLES_TABLE).execute().await.unwrap();
    let batches: Vec<RecordBatch> = tbl
        .query()
        .execute()
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let s = |b: &RecordBatch, n| {
        b.column_by_name(n)
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .clone()
    };
    let i = |b: &RecordBatch, n| {
        b.column_by_name(n)
            .unwrap()
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap()
            .clone()
    };
    let bo = |b: &RecordBatch, n| {
        b.column_by_name(n)
            .unwrap()
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap()
            .clone()
    };
    let mut out = Vec::new();
    for b in &batches {
        let (aid, title, qid) = (s(b, "atom_id"), s(b, "title"), s(b, "wikidata_qid"));
        let cid = s(b, "chunk_id");
        let (rev, pov, cit) = (
            i(b, "revision_id"),
            i(b, "pov_total"),
            i(b, "citation_total"),
        );
        let (insc, cont) = (bo(b, "in_scope"), bo(b, "is_contested"));
        for k in 0..b.num_rows() {
            out.push(WikiArticleRow {
                atom_id: aid.value(k).to_string(),
                title: title.value(k).to_string(),
                wikidata_qid: qid.value(k).to_string(),
                revision_id: rev.value(k),
                in_scope: insc.value(k),
                pov_total: pov.value(k),
                citation_total: cit.value(k),
                is_contested: cont.value(k),
                chunk_id: cid.value(k).to_string(),
            });
        }
    }
    out.sort_by(|a, b| a.title.cmp(&b.title));
    out
}

async fn read_edges(atlas_dir: &Path) -> Vec<WikiEdgeRow> {
    let db = lancedb::connect(atlas_dir.to_str().unwrap())
        .execute()
        .await
        .unwrap();
    let tbl = db.open_table(EDGES_TABLE).execute().await.unwrap();
    let batches: Vec<RecordBatch> = tbl
        .query()
        .execute()
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let s = |b: &RecordBatch, n| {
        b.column_by_name(n)
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .clone()
    };
    let i = |b: &RecordBatch, n| {
        b.column_by_name(n)
            .unwrap()
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap()
            .clone()
    };
    let bo = |b: &RecordBatch, n| {
        b.column_by_name(n)
            .unwrap()
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap()
            .clone()
    };
    let mut out = Vec::new();
    for b in &batches {
        let (src, tgt, rel, lt, sect) = (
            s(b, "source_title"),
            s(b, "target_title"),
            s(b, "relationship_type"),
            s(b, "link_text"),
            s(b, "source_section_path"),
        );
        let occ = i(b, "occurrence_count");
        let tin = bo(b, "target_in_scope");
        for k in 0..b.num_rows() {
            out.push(WikiEdgeRow {
                source_title: src.value(k).to_string(),
                target_title: tgt.value(k).to_string(),
                relationship_type: rel.value(k).to_string(),
                link_text: lt.value(k).to_string(),
                occurrence_count: occ.value(k),
                source_section_path: sect.value(k).to_string(),
                target_in_scope: tin.value(k),
            });
        }
    }
    out
}

#[tokio::test]
async fn wikipedia_columnar_store_roundtrips_articles_and_edges() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let articles = vec![art("Alpha", 0), art("Beta", 0), art("Gamma", 3)];
    // Alpha → Beta (topical, Intro), Alpha → Gamma (contested, Criticism),
    // Alpha → External (topical, See also; out-of-scope target).
    let edges = vec![
        edge("Alpha", "Beta", "topical", "Intro", 2, true),
        edge("Alpha", "Gamma", "contested", "Criticism", 1, true),
        edge("Alpha", "External", "topical", "See also", 1, false),
    ];
    write_wikipedia_columnar_store(dir, &articles, &edges)
        .await
        .unwrap();

    // articles.lance round-trips every structural column (sorted by title).
    let mut want = articles.clone();
    want.sort_by(|a, b| a.title.cmp(&b.title));
    assert_eq!(read_articles(dir).await, want);

    // edges.lance round-trips, and carries the fields the neighbor API needs.
    let re = read_edges(dir).await;
    assert_eq!(re.len(), 3);
    let alpha: Vec<&WikiEdgeRow> = re.iter().filter(|e| e.source_title == "Alpha").collect();
    assert_eq!(alpha.len(), 3);
    // axis filtering needs the section path + relationship_type on the edge.
    assert!(alpha.iter().any(|e| e.target_title == "Gamma"
        && e.relationship_type == "contested"
        && e.source_section_path == "Criticism"));
    // dangling/out-of-scope target preserved (the neighbor query filters on it).
    assert!(alpha
        .iter()
        .any(|e| e.target_title == "External" && !e.target_in_scope));
    // occurrence_count survives (the neighbor query SUMs it).
    assert_eq!(
        alpha
            .iter()
            .find(|e| e.target_title == "Beta")
            .unwrap()
            .occurrence_count,
        2
    );
}

// ── the direct build (W4) ────────────────────────────────────────────────
//
// These carried over from `wikipedia_graph::tests` when the SQLite was
// retired: they exercise the AGGREGATION, so they belong beside it. The
// one they replace, `direct_build_matches_sqlite_two_step`, asserted the
// direct build against the two-step it supersedes and is cited in the
// retirement commit; it could not survive the backend it compared against.

use crate::extractors::wikipedia_types::WikiLink;
use crate::wikipedia_columnar::{ColumnarWikipediaGraph, Neighbor};

fn meta_with(
    section_path: Vec<&str>,
    section_type: &str,
    pov_count: Option<i64>,
    outgoing: Vec<(&str, &str)>,
) -> String {
    let m = WikipediaChunkMetadata {
        section_name: section_path.last().unwrap_or(&"").to_string(),
        section_path: section_path.iter().map(|s| s.to_string()).collect(),
        section_depth: 0,
        section_type: section_type.to_string(),
        citation_needed_count: None,
        pov_count,
        clarification_needed_count: None,
        update_count: None,
        is_flagged_stable: None,
        outgoing_links: outgoing
            .into_iter()
            .map(|(t, l)| WikiLink {
                target_title: t.to_string(),
                link_text: l.to_string(),
            })
            .collect(),
        revision_id: Some(42),
        wikidata_qid: None,
        page_id: None,
    };
    serde_json::to_string(&m).unwrap()
}

fn chunk(id: u64, title: &str, metadata_raw: String) -> StoredChunkWithMetadata {
    StoredChunkWithMetadata {
        id,
        title: Some(title.to_string()),
        url: Some(format!(
            "https://en.wikipedia.org/wiki/{}",
            title.replace(' ', "_")
        )),
        metadata_raw: Some(metadata_raw),
    }
}

/// Einstein links out from a Lead and a Criticism section; Special
/// relativity links back from Origins, twice (the chunker repeating a
/// section). Photoelectric effect is linked but never a source, so it is
/// the dangling target.
fn fixture() -> Vec<StoredChunkWithMetadata> {
    vec![
        chunk(
            1,
            "Albert Einstein",
            meta_with(
                vec!["Lead"],
                "lead",
                None,
                vec![
                    ("Special relativity", "special relativity"),
                    ("Photoelectric effect", "photoelectric effect"),
                ],
            ),
        ),
        chunk(
            2,
            "Albert Einstein",
            meta_with(
                vec!["Criticism"],
                "controversy",
                Some(2),
                vec![("Special relativity", "criticism of relativity")],
            ),
        ),
        chunk(
            3,
            "Special relativity",
            meta_with(
                vec!["Origins"],
                "history",
                None,
                vec![
                    ("Albert Einstein", "Einstein"),
                    ("Photoelectric effect", "photoelectric effect"),
                ],
            ),
        ),
        chunk(
            4,
            "Special relativity",
            meta_with(
                vec!["Origins"],
                "history",
                None,
                vec![("Albert Einstein", "Einstein")],
            ),
        ),
    ]
}

/// The whole `WikipediaGraphApi` surface, from chunks through the direct
/// build to the reader, against VALUES rather than against another
/// implementation's output. Every assertion here has a nameable failing
/// input: drop `target_in_scope` and the dangling assertion goes red; lose
/// the section split and Einstein's Special-relativity occurrence falls
/// from 2 to 1; break `classify_relationship` and the `contested` label
/// goes; drop `source_section_path` from the edge row and the
/// axis-by-section case returns empty.
/// A neighbor set as sorted `(title, relationship_type, occurrence, in_scope)`
/// tuples — the whole answer, so an assertion pins what the API returns
/// rather than one field of one row.
fn rows(ns: Vec<Neighbor>) -> Vec<(String, String, i64, bool)> {
    let mut v: Vec<_> = ns
        .into_iter()
        .map(|n| (n.title, n.relationship_type, n.occurrence_count, n.in_scope))
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn direct_build_serves_the_whole_neighbor_api() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let summary = build_wikipedia_columnar_store_from_chunks(dir, "wiki-test", fixture())
        .await
        .unwrap();

    // Two in-scope sources; Photoelectric effect is a target only.
    assert_eq!(summary.articles, 2);
    assert_eq!(summary.dangling_targets, 1);
    assert_eq!(summary.chunks_with_metadata, 4);
    assert_eq!(summary.chunks_without_metadata, 0);
    // Einstein: Lead + Criticism; Special relativity: Origins (the two
    // chunks of it collapse to one section).
    assert_eq!(summary.sections, 3);
    assert_eq!(summary.revision_id_max, Some(42));

    let g = ColumnarWikipediaGraph::open(dir).await.unwrap();
    assert_eq!(g.article_count().await, 2);

    // The whole neighbor set, exactly. Grouping is by (target,
    // relationship_type) — NOT by target — so Einstein's two links to
    // Special relativity stay two rows: the Lead one classifies `topical`,
    // the Criticism one `contested`. Asserting the set rather than a field
    // is what makes that visible; an earlier draft of this test asserted
    // `occurrence_count == 2` for a summed target and was simply wrong
    // about the API.
    assert_eq!(
        rows(g.neighbors("Albert Einstein", 50).await),
        vec![
            ("Photoelectric effect".into(), "topical".into(), 1, false),
            ("Special relativity".into(), "contested".into(), 1, true),
            ("Special relativity".into(), "topical".into(), 1, true),
        ],
    );
    // The dedupe: Special relativity's Origins section is two chunks
    // repeating the same links, and collapses to one edge each. The
    // `Origins` path classifies both `causal`.
    assert_eq!(
        rows(g.neighbors("Special relativity", 50).await),
        vec![
            ("Albert Einstein".into(), "causal".into(), 1, true),
            ("Photoelectric effect".into(), "causal".into(), 1, false),
        ],
    );
    // reverse_neighbors is the in-edge view of the same edges. Note the
    // `in_scope` flag flips meaning with the direction: here the returned
    // article is the SOURCE, and both sources are in-scope, so both are
    // true even though the article being asked about is dangling.
    assert_eq!(
        rows(g.reverse_neighbors("Photoelectric effect", 50).await),
        vec![
            ("Albert Einstein".into(), "topical".into(), 1, true),
            ("Special relativity".into(), "causal".into(), 1, true),
        ],
    );

    // The contested signal rides on the article, from the Criticism section.
    assert!(g.has_contested_section("Albert Einstein").await);
    assert!(!g.has_contested_section("Special relativity").await);

    let rec = g.record("Albert Einstein").await.expect("record");
    assert_eq!(rec.title, "Albert Einstein");
    assert!(rec.in_scope);
    assert_eq!(rec.pov_total, 2);
    assert_eq!(rec.revision_id, Some(42));
}

/// The axis filter, one term per column it matches. These three columns are
/// the reason the wiki store is `edges.lance` and not the `edges.csr`
/// adjacency — a 10-byte CSR record has nowhere to put a section path or a
/// link text.
#[tokio::test]
async fn axis_filter_matches_section_path_link_text_and_target_title() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    build_wikipedia_columnar_store_from_chunks(dir, "wiki-test", fixture())
        .await
        .unwrap();
    let g = ColumnarWikipediaGraph::open(dir).await.unwrap();

    let hits = |ns: &[Neighbor]| {
        let mut v: Vec<String> = ns.iter().map(|n| n.title.clone()).collect();
        v.sort();
        v
    };

    // (a) matches `source_section_path` — only the Criticism-section link.
    let by_section = g
        .neighbors_for_axis("Albert Einstein", &["criticism".to_string()], 50)
        .await;
    assert_eq!(hits(&by_section), vec!["Special relativity".to_string()]);
    // That link was classified from its section, so it carries the label.
    assert_eq!(by_section[0].relationship_type, "contested");

    // (b) matches `link_text` — "Einstein" is anchor text on Special
    //     relativity's out-edge, and appears in no section path here.
    let by_text = g
        .neighbors_for_axis("Special relativity", &["einstein".to_string()], 50)
        .await;
    assert_eq!(hits(&by_text), vec!["Albert Einstein".to_string()]);

    // (c) matches `target_title`.
    let by_title = g
        .neighbors_for_axis("Albert Einstein", &["photoelectric".to_string()], 50)
        .await;
    assert_eq!(hits(&by_title), vec!["Photoelectric effect".to_string()]);

    // A term matching nothing returns nothing — the failing input that
    // makes the three above mean something.
    let miss = g
        .neighbors_for_axis("Albert Einstein", &["zeppelin".to_string()], 50)
        .await;
    assert!(miss.is_empty());

    // co_neighbors: the concept both articles reach.
    let both = vec![
        "Albert Einstein".to_string(),
        "Special relativity".to_string(),
    ];
    let shared = g.co_neighbors(&both, &[], 50).await;
    assert_eq!(hits(&shared), vec!["Photoelectric effect".to_string()]);
}

/// The classifier's section-path rules beat its link-text rules, and the
/// default is `topical`. Kept from `wikipedia_graph::tests` — the function
/// moved here, so its test did too.
#[test]
fn relationship_classifier_orders_section_over_link_text() {
    // Section path wins even when the link text says "is".
    assert_eq!(
        classify_relationship(&["Criticism".to_string()], "is a physicist"),
        "contested"
    );
    assert_eq!(
        classify_relationship(&["Origins".to_string()], "Industrial Revolution"),
        "causal"
    );
    assert_eq!(
        classify_relationship(&["See also".to_string()], "anything"),
        "see-also"
    );
    // No section signal → link-text verb prefixes.
    assert_eq!(
        classify_relationship(&[], "led to widespread famine"),
        "causal"
    );
    assert_eq!(classify_relationship(&[], "is a mammal"), "defines");
    // Neither → topical.
    assert_eq!(
        classify_relationship(&["Lead".to_string()], "Vienna"),
        "topical"
    );
}

/// A chunk with no metadata, and one with metadata but no resolvable
/// title, are both counted and neither crashes the build.
#[tokio::test]
async fn chunks_without_usable_metadata_are_counted_not_dropped_silently() {
    let mut chunks = fixture();
    chunks.push(StoredChunkWithMetadata {
        id: 99,
        title: Some("No Metadata".into()),
        url: None,
        metadata_raw: None,
    });
    chunks.push(StoredChunkWithMetadata {
        id: 100,
        title: Some("Bad Metadata".into()),
        url: None,
        metadata_raw: Some("{not json".into()),
    });
    let (_, _, summary) = wiki_rows_from_chunks("wiki-test", chunks).unwrap();
    assert_eq!(summary.chunks_with_metadata, 4);
    assert_eq!(summary.chunks_without_metadata, 2);
    assert_eq!(summary.articles, 2);
}

/// The atom id is a function of the article and its corpus, and of nothing
/// else — not of position, not of insertion order, not of the rest of the
/// corpus. That is the whole difference from the counter ids it replaces
/// (`entity-0001`…, assigned in sorted-title order, so inserting one
/// article shifted every id after it).
#[test]
fn wiki_atom_id_depends_on_the_article_and_the_corpus_and_nothing_else() {
    let a = wiki_atom_id("Roman Empire", "wikipedia");
    assert_eq!(a, wiki_atom_id("Roman Empire", "wikipedia"));
    assert!(a.starts_with("entity-"));
    assert_eq!(a.len(), "entity-".len() + 16);
    // Same title, different corpus → different atom. This is what makes an
    // id unique across the federation without a registry.
    assert_ne!(a, wiki_atom_id("Roman Empire", "wikipedia-newsworthy"));
    // Different title, same corpus → different atom.
    assert_ne!(a, wiki_atom_id("Roman Republic", "wikipedia"));
    // ONE DERIVATION: this must BE `exact_entity_content_hash`, not merely
    // agree with it today. A second hasher for one essence is the §10.6
    // smell, and it is what an earlier draft of this function was.
    assert_eq!(
        a,
        AtomId::exact_entity_content_hash(
            "Roman Empire",
            &EntityType::from_str_repr(WIKI_ENTITY_TYPE),
            "wikipedia"
        )
        .as_str()
    );
    // Field boundaries can't be smuggled through a title: the constructor
    // length-frames each field. These two would collide under a naive
    // `"{title}|{corpus}"` join.
    assert_ne!(wiki_atom_id("a|b", "c"), wiki_atom_id("a", "b|c"));
    // A WIKIPEDIA TITLE IS AN IDENTIFIER, NOT A DESCRIPTION. Case and
    // punctuation are part of it, and this assertion is the inversion of
    // what stood here before 2026-09-04, when the id folded through
    // `lookup_key` and these three titles were one atom.
    assert_ne!(
        wiki_atom_id("Roman Empire", "w"),
        wiki_atom_id("roman  empire!", "w")
    );
    assert_ne!(
        wiki_atom_id("Roman Empire", "w"),
        wiki_atom_id("Roman empire", "w")
    );
    // The folded constructor is what could not tell them apart. Asserted
    // so this test speaks up if `lookup_key` ever stops folding and the
    // delegation above becomes a distinction without a difference.
    assert_eq!(
        AtomId::entity_content_hash(
            "Roman Empire",
            &EntityType::from_str_repr(WIKI_ENTITY_TYPE),
            "w"
        ),
        AtomId::entity_content_hash(
            "roman  empire!",
            &EntityType::from_str_repr(WIKI_ENTITY_TYPE),
            "w"
        ),
    );
}

/// The progressive path keeps working across the re-key.
///
/// `wikipedia-fetched` and `wikipedia-newsworthy` write their OWN
/// atom-class atlases (413 and 14 atoms on this host, all content-hash
/// ids already) and reach their `wikipedia` twin by atom id — a
/// `CrossCorpusEdge` carries `peer.corpus_id` + `peer.atom_id`. So the id a
/// layer mints for "Roman Empire in wikipedia", using the shared
/// `exact_entity_content_hash` and knowing nothing about this module, must
/// be the id the rebuilt wiki store holds. That is what makes the edge
/// resolve, and it is why `wiki_atom_id` delegates rather than agreeing.
/// The constructor a peer must call changed on 2026-09-04 (folded → exact);
/// the property did not.
///
/// The failing input is the previous scheme: a counter id assigned in
/// sorted-title order can never be computed by a peer, so no cross-corpus
/// edge into wikipedia could have resolved by construction.
#[tokio::test]
async fn an_id_minted_by_a_peer_layer_resolves_in_the_rebuilt_wiki_store() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    build_wikipedia_columnar_store_from_chunks(
        dir,
        "wikipedia",
        vec![chunk(
            3,
            "Roman Empire",
            meta_with(vec!["Lead"], "lead", None, vec![("Augustus", "Augustus")]),
        )],
    )
    .await
    .unwrap();

    // What a peer layer computes for the SAME article in the parent
    // corpus, through the shared function, with no reference to this file.
    let peer_minted = AtomId::exact_entity_content_hash(
        "Roman Empire",
        &EntityType::from_str_repr("article"),
        "wikipedia",
    );

    use crate::enrichment::atlas::provider::AtlasProvider;
    let p = crate::wikipedia_columnar::WikiAtlasProvider::open(dir, "wikipedia")
        .await
        .unwrap();
    let hit = p
        .atom(peer_minted.as_str())
        .expect("a peer-minted id must resolve in the wiki store");
    assert_eq!(hit.name(), "Roman Empire");
    assert_eq!(p.atom_evidence(peer_minted.as_str())[0].chunk_id(), "3");

    // And the layer's own corpus gives a DIFFERENT id for the same title,
    // which is the point of corpus-qualifying: the twin is a distinct atom
    // in a distinct corpus, reached by an edge rather than confused with it.
    assert_ne!(
        peer_minted.as_str(),
        wiki_atom_id("Roman Empire", "wikipedia-fetched")
    );
}

/// THE FAILING INPUT, from production, not from imagination.
///
/// The first full wikipedia rebuild (2026-09-04, unit `ei-7c-wiki-rebuild`,
/// 1,896,488 chunk records streamed, rc=1) died here:
///
/// ```text
/// error: build: wiki atom id collision: entity-6b8aef7e6205164a is both
/// "Jigsaw puzzle" and "Jigsaw Puzzle" in corpus wikipedia
/// ```
///
/// Two real encyclopedia articles, one atom id, because the id folded
/// through `canonical::lookup_key`. This test is that build in miniature:
/// before `wiki_atom_id` delegated to the exact constructor it returned
/// `Err(collision)` on these two chunks; the guard did its job (§18.1) and
/// the id was the defect. Not a one-off — the same fold merges 40,869 of
/// the corpus's 1,562,311 titles.
#[test]
fn two_wikipedia_titles_differing_only_in_case_build_two_articles() {
    let chunks = vec![
        chunk(
            11,
            "Jigsaw puzzle",
            meta_with(vec!["Lead"], "lead", None, vec![("Puzzle", "puzzle")]),
        ),
        chunk(
            12,
            "Jigsaw Puzzle",
            meta_with(
                vec!["Lead"],
                "lead",
                None,
                vec![("The Rolling Stones", "the Rolling Stones")],
            ),
        ),
    ];
    let (articles, _edges, _dangling) = wiki_rows_from_chunks("wikipedia", chunks)
        .expect("two distinct titles must not collide into one atom");
    assert_eq!(articles.len(), 2, "one row per article, not one merged row");

    let lower = articles
        .iter()
        .find(|a| a.title == "Jigsaw puzzle")
        .expect("the lowercase article survives");
    let upper = articles
        .iter()
        .find(|a| a.title == "Jigsaw Puzzle")
        .expect("the capitalised article survives");
    assert_ne!(lower.atom_id, upper.atom_id);
    assert_eq!(lower.atom_id, wiki_atom_id("Jigsaw puzzle", "wikipedia"));
    assert_eq!(upper.atom_id, wiki_atom_id("Jigsaw Puzzle", "wikipedia"));
    // Each keeps its own evidence anchor — the merge would have thrown one
    // article's chunk away, which is what makes a silent merge worse than
    // a refused build.
    assert_eq!(lower.chunk_id, "11");
    assert_eq!(upper.chunk_id, "12");
}

/// The evidence anchor is the LOWEST chunk id of the article, so a rebuild
/// picks the same chunk whatever order the index streams rows in. The
/// fixture feeds the higher id first for exactly that reason.
#[tokio::test]
async fn chunk_anchor_is_the_lowest_chunk_not_the_first_seen() {
    let chunks = vec![
        chunk(
            900,
            "Albert Einstein",
            meta_with(vec!["Later"], "body", None, vec![("X", "x")]),
        ),
        chunk(
            7,
            "Albert Einstein",
            meta_with(vec!["Lead"], "lead", None, vec![("Y", "y")]),
        ),
    ];
    let (articles, _, _) = wiki_rows_from_chunks("wikipedia", chunks).unwrap();
    assert_eq!(articles.len(), 1);
    assert_eq!(articles[0].chunk_id, "7");
    assert_eq!(
        articles[0].atom_id,
        wiki_atom_id("Albert Einstein", "wikipedia")
    );
}

/// A v1 store — no `atom_id`, no `chunk_id` — still serves the neighbor API
/// and says so about the walk. The failing input is a real one: it is the
/// shape of every `articles.lance` written before 2026-09-04, including the
/// installed wikipedia index at the time of the change.
#[tokio::test]
async fn a_v1_store_serves_neighbors_and_reports_that_it_cannot_serve_the_walk() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    // The v1 articles schema, verbatim: seven columns, no atom_id/chunk_id.
    let v1 = Arc::new(Schema::new(vec![
        Field::new("title", DataType::Utf8, false),
        Field::new("wikidata_qid", DataType::Utf8, false),
        Field::new("revision_id", DataType::Int64, false),
        Field::new("in_scope", DataType::Boolean, false),
        Field::new("pov_total", DataType::Int64, false),
        Field::new("citation_total", DataType::Int64, false),
        Field::new("is_contested", DataType::Boolean, false),
    ]));
    let batch = RecordBatch::try_new(
        v1.clone(),
        vec![
            Arc::new(StringArray::from(vec!["Alpha", "Beta"])) as arrow_array::ArrayRef,
            Arc::new(StringArray::from(vec!["", ""])),
            Arc::new(Int64Array::from(vec![-1i64, -1])),
            Arc::new(BooleanArray::from(vec![true, true])),
            Arc::new(Int64Array::from(vec![0i64, 0])),
            Arc::new(Int64Array::from(vec![0i64, 0])),
            Arc::new(BooleanArray::from(vec![false, false])),
        ],
    )
    .unwrap();
    write_table(
        dir,
        ARTICLES_LANCE_DIRNAME,
        ARTICLES_TABLE,
        v1,
        vec![batch],
        None,
    )
    .await
    .unwrap();
    // Edges are unchanged between v1 and v2, so the real writer serves.
    let esch = edges_schema();
    let erows = vec![edge("Alpha", "Beta", "topical", "Lead", 1, true)];
    write_table(
        dir,
        EDGES_LANCE_DIRNAME,
        EDGES_TABLE,
        esch.clone(),
        vec![edges_batch(&erows, &esch).unwrap()],
        Some("source_title"),
    )
    .await
    .unwrap();

    let g = ColumnarWikipediaGraph::open(dir).await.unwrap();
    // It knows what it is …
    assert!(!g.has_v2_columns());
    // … and the neighbor API is entirely unaffected by the missing columns.
    let n = g.neighbors("Alpha", 10).await;
    assert_eq!(n.len(), 1);
    assert_eq!(n[0].title, "Beta");
    assert!(g.record("Alpha").await.is_some());

    // A store the current writer produces answers the other way.
    let tmp2 = tempfile::tempdir().unwrap();
    build_wikipedia_columnar_store_from_chunks(tmp2.path(), "wiki-test", fixture())
        .await
        .unwrap();
    assert!(ColumnarWikipediaGraph::open(tmp2.path())
        .await
        .unwrap()
        .has_v2_columns());
}

// ── the borrowed ANN seed table ─────────────────────────────────────────────

/// A tiny two-chunk corpus index whose chunk ids are 1 and 2, so the wiki
/// store's `chunk_id` anchors line up with real rows. `only_first` inserts
/// chunk 1 alone — the shortfall case.
async fn tiny_index(dir: &Path, only_first: bool) -> crate::index::CorpusIndex {
    use crate::index::{InsertChunk, InsertCodeMeta};
    let idx = crate::index::CorpusIndex::create(
        &dir.join("wiki-test"),
        "wiki-test",
        "Wiki Test",
        "test-model",
        4,
        false,
        "MIT",
    )
    .await
    .expect("create index");
    let mk = |content: &str, v: [f32; 4]| {
        (
            InsertChunk {
                content: content.into(),
                title: None,
                url: None,
                metadata: None,
                content_hash: None,
                source_doc_id: None,
                source_file: None,
                code: InsertCodeMeta::default(),
                unit_id: None,
            },
            v.to_vec(),
        )
    };
    let mut rows = vec![mk("alpha lead passage", [1.0, 0.0, 0.0, 0.0])];
    if !only_first {
        rows.push(mk("beta lead passage", [0.0, 1.0, 0.0, 0.0]));
    }
    idx.insert_batch(&rows).await.expect("insert");
    idx
}

/// Two articles, anchored to chunks 1 and 2 — the ids `tiny_index` allocates.
async fn two_article_store(atlas_dir: &Path) {
    build_wikipedia_columnar_store_from_chunks(
        atlas_dir,
        "wiki-test",
        vec![
            chunk(
                1,
                "Alpha",
                meta_with(vec!["Lead"], "lead", None, vec![("Beta", "beta")]),
            ),
            chunk(2, "Beta", meta_with(vec!["Lead"], "lead", None, vec![])),
        ],
    )
    .await
    .expect("build wiki store");
}

/// EACH ATOM GETS ITS OWN ARTICLE'S CHUNK VECTOR.
///
/// The whole migration is a join, so the only way it can be wrong and still
/// look right is by borrowing the WRONG row — every atom would still get a
/// real 1024-d vector from a real article, the table would still have the
/// right row count, and the walk would seed on other people's articles. So
/// this asserts the pairing, not the count: search the written table with
/// Alpha's chunk vector and Alpha's atom must come back holding exactly it.
///
/// Failing input: swap the two `chunk(...)` ids in `two_article_store`.
#[tokio::test]
async fn the_borrowed_seed_table_gives_each_atom_its_own_articles_vector() {
    use crate::enrichment::atlas::ann_store::{ann_table_present, AnnSeedTable};
    let tmp = tempfile::tempdir().unwrap();
    let atlas = tmp.path().join("atlas");
    std::fs::create_dir_all(&atlas).unwrap();
    two_article_store(&atlas).await;
    let idx = tiny_index(tmp.path(), false).await;

    let stats = build_borrowed_ann_seed_table(&atlas, &idx, "wiki-test")
        .await
        .expect("borrowed seed table");
    assert_eq!(
        stats,
        BorrowedSeedStats {
            articles: 2,
            with_chunk_anchor: 2,
            resolved: 2,
            written: 2,
        }
    );
    assert!(
        ann_table_present(&atlas),
        "the walk gates on the table's PRESENCE in the atlas dir; a build that \
         writes it somewhere else seeds nothing"
    );

    let table = AnnSeedTable::open_for_atlas(&atlas).await.unwrap();
    let hits = table
        .nearest_with_vectors(&[1.0, 0.0, 0.0, 0.0], 2)
        .await
        .unwrap();
    let (key, vector) = hits.first().expect("a nearest hit");
    assert_eq!(
        key,
        &wiki_atom_id("Alpha", "wiki-test"),
        "Alpha's chunk vector must lead back to Alpha's atom, not Beta's"
    );
    assert_eq!(
        vector.as_slice(),
        [1.0_f32, 0.0, 0.0, 0.0].as_slice(),
        "the stored seed must BE the chunk's vector — borrowed, not re-derived"
    );
}

/// An article whose chunk has no row is a SHORTFALL, reported by number, not
/// a zero vector and not a silent drop of the whole build (ARCH §18.3).
///
/// This is the shape a partially-reingested corpus takes, and the two numbers
/// have to stay separable: `with_chunk_anchor` says the store had a join key,
/// `resolved` says the index answered it. One number could not tell a
/// chunk-less article from an unresolvable chunk.
#[tokio::test]
async fn an_article_whose_chunk_is_missing_is_counted_not_invented() {
    let tmp = tempfile::tempdir().unwrap();
    let atlas = tmp.path().join("atlas");
    std::fs::create_dir_all(&atlas).unwrap();
    two_article_store(&atlas).await;
    let idx = tiny_index(tmp.path(), true).await; // chunk 2 never inserted

    let stats = build_borrowed_ann_seed_table(&atlas, &idx, "wiki-test")
        .await
        .expect("a partial join still writes what it resolved");
    assert_eq!(stats.with_chunk_anchor, 2, "both articles had a join key");
    assert_eq!(stats.resolved, 1, "only one of them resolved to a vector");
    assert_eq!(stats.written, 1);
    assert!(
        stats.describe().contains("1 of those"),
        "the operator line must name the shortfall: {}",
        stats.describe()
    );
}
