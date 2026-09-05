//! Tests for [`super`] — carved out of `wikipedia_columnar.rs` unchanged (ARCH §3.1:
//! the file crossed the 1200-line ceiling). Mechanical move only: no
//! assertion, fixture or name differs from the inline module it replaces.

use super::*;
use crate::enrichment::atlas::wiki_store::{
    wiki_atom_id, write_wikipedia_columnar_store, WikiArticleRow, WikiEdgeRow,
    ARTICLES_LANCE_DIRNAME,
};

fn art(title: &str, contested: bool) -> WikiArticleRow {
    WikiArticleRow {
        atom_id: wiki_atom_id(title, "wiki-test"),
        title: title.into(),
        wikidata_qid: format!("Q-{title}"),
        revision_id: 10,
        in_scope: true,
        pov_total: if contested { 2 } else { 0 },
        citation_total: 3,
        is_contested: contested,
        chunk_id: String::new(),
    }
}

fn edge(src: &str, tgt: &str, rel: &str, sect: &str, occ: i64) -> WikiEdgeRow {
    WikiEdgeRow {
        source_title: src.into(),
        target_title: tgt.into(),
        relationship_type: rel.into(),
        link_text: tgt.to_lowercase(),
        occurrence_count: occ,
        source_section_path: sect.into(),
        target_in_scope: true,
    }
}

#[tokio::test]
async fn columnar_graph_serves_the_neighbor_api() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    // A,B,D in scope; C contested + in scope. Links:
    //   A→B (topical/Intro), A→C (contested/Criticism), A→D (topical/See also)
    //   B→C (topical/Body)  — so C is co-cited by A and B.
    let articles = vec![
        art("A", false),
        art("B", false),
        art("C", true),
        art("D", false),
    ];
    let edges = vec![
        edge("A", "B", "topical", "Intro", 2),
        edge("A", "C", "contested", "Criticism", 1),
        edge("A", "D", "topical", "See also", 1),
        edge("B", "C", "topical", "Body", 3),
    ];
    write_wikipedia_columnar_store(dir, &articles, &edges)
        .await
        .unwrap();
    let g = ColumnarWikipediaGraph::open(dir).await.unwrap();

    // neighbors(A): B, C, D — ranked by occurrence (B=2 first).
    let n = g.neighbors("A", 10).await;
    assert_eq!(n.len(), 3);
    assert_eq!(n[0].title, "B");
    assert_eq!(n[0].occurrence_count, 2);
    let titles: HashSet<&str> = n.iter().map(|x| x.title.as_str()).collect();
    assert!(titles.contains("C") && titles.contains("D"));

    // neighbors_for_axis(A, ["criticism"]): only C (Criticism section).
    let ax = g.neighbors_for_axis("A", &["criticism".into()], 10).await;
    assert_eq!(ax.len(), 1);
    assert_eq!(ax[0].title, "C");
    assert_eq!(ax[0].relationship_type, "contested");

    // co_neighbors(A, B): C, linked from both.
    let co = g.co_neighbors(&["A".into(), "B".into()], &[], 10).await;
    assert_eq!(co.len(), 1);
    assert_eq!(co[0].title, "C");
    assert_eq!(co[0].occurrence_count, 4); // 1 (A→C) + 3 (B→C)

    // reverse_neighbors(C): A and B link to it.
    let rev = g.reverse_neighbors("C", 10).await;
    let rtitles: HashSet<&str> = rev.iter().map(|x| x.title.as_str()).collect();
    assert!(rtitles.contains("A") && rtitles.contains("B"));

    // has_contested_section + record.
    assert!(g.has_contested_section("C").await);
    assert!(!g.has_contested_section("A").await);
    let rec = g.record("A").await.expect("record A");
    assert_eq!(rec.title, "A");
    assert_eq!(rec.wikidata_qid.as_deref(), Some("Q-A"));
    assert!(rec.in_scope);
    assert_eq!(rec.pov_total, 0);
    assert!(g.record("nope").await.is_none());
}

/// The gate: `open_wikipedia_graph` returns `None` when no store is present,
/// and selects the columnar backend once `atlas/articles.lance` +
/// `atlas/edges.lance` exist for the corpus.
#[tokio::test]
async fn open_wikipedia_graph_serves_the_columnar_store_and_nothing_else() {
    use crate::enrichment::atlas::wiki_store::{
        wiki_atom_id, write_wikipedia_columnar_store, WikiArticleRow, WikiEdgeRow,
    };
    let tmp = tempfile::tempdir().unwrap();
    let indexes_dir = tmp.path();
    let corpus_id = "wiki-test";

    // Nothing present yet → None. Since W4 there is no SQLite to fall to,
    // so this is the whole of "no link graph for this corpus".
    assert!(open_wikipedia_graph(indexes_dir, corpus_id).await.is_none());

    // Write a columnar store at <indexes>/<corpus>/atlas/.
    let atlas_dir = indexes_dir
        .join(corpus_id)
        .join(crate::enrichment::atlas::ATLAS_DIRNAME);
    std::fs::create_dir_all(&atlas_dir).unwrap();
    let articles = vec![
        WikiArticleRow {
            atom_id: wiki_atom_id("A", "wiki-test"),
            title: "A".into(),
            wikidata_qid: String::new(),
            revision_id: -1,
            in_scope: true,
            pov_total: 0,
            citation_total: 0,
            is_contested: false,
            chunk_id: String::new(),
        },
        WikiArticleRow {
            atom_id: wiki_atom_id("B", "wiki-test"),
            title: "B".into(),
            wikidata_qid: String::new(),
            revision_id: -1,
            in_scope: true,
            pov_total: 0,
            citation_total: 0,
            is_contested: false,
            chunk_id: String::new(),
        },
    ];
    let edges = vec![WikiEdgeRow {
        source_title: "A".into(),
        target_title: "B".into(),
        relationship_type: "topical".into(),
        link_text: "b".into(),
        occurrence_count: 1,
        source_section_path: "Lead".into(),
        target_in_scope: true,
    }];
    write_wikipedia_columnar_store(&atlas_dir, &articles, &edges)
        .await
        .unwrap();

    // With the store present the gate serves neighbors from it.
    let g = open_wikipedia_graph(indexes_dir, corpus_id)
        .await
        .expect("columnar graph selected");
    let n = g.neighbors("A", 10).await;
    assert_eq!(n.len(), 1);
    assert_eq!(n[0].title, "B");
}

// ── the walk's face on the same store ────────────────────────────────────

/// Fixture chunks -> the direct build -> the provider, asserted against
/// VALUES. Every question the walk asks, answered from `articles.lance` +
/// `edges.lance` with no atom store anywhere.
#[tokio::test]
async fn provider_answers_the_walk_from_the_wiki_store() {
    use crate::enrichment::atlas::wiki_store::{
        build_wikipedia_columnar_store_from_chunks, wiki_atom_id,
    };
    use crate::extractors::wikipedia_types::WikiLink;
    use crate::index::StoredChunkWithMetadata;

    fn meta(section: &str, kind: &str, links: Vec<(&str, &str)>) -> String {
        let m = crate::extractors::wikipedia_types::WikipediaChunkMetadata {
            section_name: section.into(),
            section_path: vec![section.into()],
            section_depth: 0,
            section_type: kind.into(),
            citation_needed_count: None,
            pov_count: None,
            clarification_needed_count: None,
            update_count: None,
            is_flagged_stable: None,
            outgoing_links: links
                .into_iter()
                .map(|(t, l)| WikiLink {
                    target_title: t.into(),
                    link_text: l.into(),
                })
                .collect(),
            revision_id: Some(1),
            wikidata_qid: None,
            page_id: None,
        };
        serde_json::to_string(&m).unwrap()
    }
    fn ch(id: u64, title: &str, m: String) -> StoredChunkWithMetadata {
        StoredChunkWithMetadata {
            id,
            title: Some(title.into()),
            url: None,
            metadata_raw: Some(m),
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    // A -> B (in scope, both ways) and A -> Dangling (target never a source).
    build_wikipedia_columnar_store_from_chunks(
        dir,
        "wikipedia",
        vec![
            ch(
                5,
                "Alpha",
                meta("Lead", "lead", vec![("Beta", "beta"), ("Dangling", "d")]),
            ),
            ch(2, "Beta", meta("Lead", "lead", vec![("Alpha", "alpha")])),
        ],
    )
    .await
    .unwrap();

    let p = WikiAtlasProvider::open(dir, "wikipedia").await.unwrap();
    let alpha = wiki_atom_id("Alpha", "wikipedia");
    let beta = wiki_atom_id("Beta", "wikipedia");

    assert_eq!(p.atlas_corpus_id(), "wikipedia");
    // Self-hosted: the atlas and its chunks share one index, so no title
    // filter applies and the slug is the corpus.
    assert_eq!(p.site().chunk_corpus().as_str(), "wikipedia");
    assert_eq!(p.site().article(), None);
    assert_eq!(p.article_slug(), "wikipedia");

    // atom(): identity, name, kind, subtype.
    let a = p.atom(&alpha).expect("Alpha is an atom");
    assert_eq!(a.id(), alpha);
    assert_eq!(a.name(), "Alpha");
    assert_eq!(a.kind(), AtomType::Entity);
    assert_eq!(a.subtype(), "article");
    // No payload, so no deep read to re-parse — that is the wiki shape.
    assert!(a.atom_envelope().is_none());
    // The dangling target is NOT an atom: it owns no chunk, so it could
    // never become a citation.
    assert!(p.atom(&wiki_atom_id("Dangling", "wikipedia")).is_none());
    assert!(p.atom("entity-deadbeefdeadbeef").is_none());
    assert_eq!(p.atom_count(), 2);

    // atom_evidence(): the article's lowest chunk id, as a citation anchor.
    let ev = p.atom_evidence(&alpha);
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].chunk_id(), "5");
    assert_eq!(p.atom_evidence(&beta)[0].chunk_id(), "2");
    assert!(p.atom_evidence("entity-deadbeefdeadbeef").is_empty());

    // edges_from/to(): endpoints are ATOM IDS, kind is the closed
    // `Involves`, and the edge to the dangling target is absent because it
    // has no atom id to name.
    let out = p.edges_from(&alpha);
    assert_eq!(out.len(), 1, "only the in-scope target survives");
    assert_eq!(out[0].source, alpha);
    assert_eq!(out[0].target, beta);
    assert_eq!(out[0].edge_type, EdgeType::Involves);
    assert_eq!(out[0].provenance, EdgeProvenance::WikilinkStructural);
    let back = p.edges_to(&alpha);
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].source, beta);
    assert_eq!(back[0].target, alpha);
    assert_eq!(p.edge_count(), 2);

    // The same store still answers the neighbor face, dangling target and
    // relationship label included — that is the point of one store, two
    // faces.
    let g = ColumnarWikipediaGraph::open(dir).await.unwrap();
    let ns = g.neighbors("Alpha", 10).await;
    assert_eq!(ns.len(), 2);
    assert!(ns.iter().any(|n| n.title == "Dangling" && !n.in_scope));
    assert!(ns.iter().all(|n| n.relationship_type == "topical"));

    // No seed table until one is migrated; the walk names that rather than
    // reading it as an empty result.
    assert!(!p.has_ann_seed_table());
    assert!(p.ann_seed_table().is_none());
    // Nothing declared, so every declared-type path stays inert.
    assert!(p.ontology().is_none());
    assert!(!p.is_subtype_of("article", "anything"));
}

/// A v1 store is REFUSED by name rather than serving atoms with no identity
/// and no evidence. The failing input is every `articles.lance` written
/// before 2026-09-04, the installed wikipedia index included.
#[tokio::test]
async fn provider_refuses_a_v1_store_by_name() {
    use crate::enrichment::atlas::wiki_store::{
        write_wikipedia_columnar_store, WikiArticleRow, WikiEdgeRow,
    };
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    // Write a current store, then strip the v2 columns by rewriting
    // articles.lance through the v1 schema.
    write_wikipedia_columnar_store(
        dir,
        &[WikiArticleRow {
            atom_id: "entity-0000000000000000".into(),
            title: "Alpha".into(),
            wikidata_qid: String::new(),
            revision_id: -1,
            in_scope: true,
            pov_total: 0,
            citation_total: 0,
            is_contested: false,
            chunk_id: "1".into(),
        }],
        &[WikiEdgeRow {
            source_title: "Alpha".into(),
            target_title: "Beta".into(),
            relationship_type: "topical".into(),
            link_text: "beta".into(),
            occurrence_count: 1,
            source_section_path: "Lead".into(),
            target_in_scope: false,
        }],
    )
    .await
    .unwrap();
    // v2 opens.
    assert!(WikiAtlasProvider::open(dir, "wikipedia").await.is_ok());

    // Now the v1 shape.
    std::fs::remove_dir_all(dir.join(ARTICLES_LANCE_DIRNAME)).unwrap();
    let v1 = Arc::new(arrow::datatypes::Schema::new(vec![
        arrow::datatypes::Field::new("title", arrow::datatypes::DataType::Utf8, false),
    ]));
    let batch = RecordBatch::try_new(
        v1.clone(),
        vec![Arc::new(StringArray::from(vec!["Alpha"])) as arrow_array::ArrayRef],
    )
    .unwrap();
    let db = lancedb::connect(dir.to_str().unwrap())
        .execute()
        .await
        .unwrap();
    let t = db
        .create_empty_table(ARTICLES_TABLE, v1)
        .execute()
        .await
        .unwrap();
    t.add(vec![batch]).execute().await.unwrap();

    let Err(err) = WikiAtlasProvider::open(dir, "wikipedia").await else {
        panic!("a v1 store must be refused, not served");
    };
    assert!(
        err.contains("format v1"),
        "error must name the cause: {err}"
    );
    assert!(
        err.contains("build-graph"),
        "error must name the repair: {err}"
    );
}

/// EMPTY AND ABSENT ARE DIFFERENT FACTS, and `record` is what tells them apart.
///
/// `neighbors` returns an empty vec for both "this article has no outgoing
/// links in the store" and "there is no such article", and for a while the
/// `neighbors` CLI verb printed the same thing for each and asked the reader to
/// guess. That guess was made wrong at least once: a 0-result query against a
/// rebuilt wikipedia store was explained as "present, no edges" when the real
/// cause was reading a different store entirely.
///
/// This pins the property the verb now relies on. It is a CHARACTERISATION
/// test, not a watched-failing one: `record` already behaved this way, and what
/// changed on 2026-09-04 was that the caller started asking it. Its job is to
/// make the behaviour break loudly if `record` is ever narrowed to "articles
/// that have edges".
#[tokio::test]
async fn record_distinguishes_an_edgeless_article_from_an_absent_one() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    // "Lonely" is a real in-scope article that no edge mentions — the shape of
    // a stub, and of the Rolling Stones song that started this.
    let articles = vec![art("Linked", false), art("Lonely", false)];
    let edges = vec![edge("Linked", "Elsewhere", "topical", "Intro", 1)];
    write_wikipedia_columnar_store(dir, &articles, &edges)
        .await
        .unwrap();
    let g = ColumnarWikipediaGraph::open(dir).await.unwrap();

    // Both return NO neighbors — this is the ambiguity, reproduced.
    assert!(g.neighbors("Lonely", 10).await.is_empty());
    assert!(g.neighbors("Nonexistent Article", 10).await.is_empty());

    // And `record` separates them, which is the whole point.
    assert!(
        g.record("Lonely").await.is_some(),
        "an edgeless article is PRESENT and must be reported as present"
    );
    assert!(
        g.record("Nonexistent Article").await.is_none(),
        "an absent title must be reported as absent, not as an empty result"
    );

    // Case still matters here as everywhere in this store.
    assert!(g.record("lonely").await.is_none());
}
