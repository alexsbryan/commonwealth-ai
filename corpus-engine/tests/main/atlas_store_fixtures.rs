// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one writer of the atlas store fixtures checked in under
//! shared/crates/corpus-engine-atlas-reader/testdata/stores (phase-b-48).
//!
//! svrn's `AtlasContextManager` tests open those stores through the leaf's
//! openers. Here ingest writes the same inputs fresh and the leaf's opener
//! must read the fresh store and the checked-in one identically; a store
//! format change turns this red, which is the point. Regenerate with
//! `regenerate_atlas_store_fixtures` (`#[ignore]`d: it rewrites checked-in
//! files).

use std::path::Path;

use corpus_engine::enrichment::atlas::store::write_store_blocking;
use corpus_engine::enrichment::atlas::wiki_store::{
    build_wikipedia_columnar_store_from_chunks, wiki_atom_id,
};
use corpus_engine::extractors::wikipedia_types::{WikiLink, WikipediaChunkMetadata};
use corpus_engine_atlas_reader::fixtures::{
    copy_store_fixture, store_fixture_dir, ATOM_STORE, WIKI_ALPHA_ATOM_ID, WIKI_STORE,
};
use corpus_engine_atlas_reader::opener::open_walk_provider_blocking;
use corpus_index::index::StoredChunkWithMetadata;
use understanding_vocab::read::{read_atlas_atoms, ATLAS_DIRNAME};

/// `atoms.json` with no atoms: the atom store's input.
const EMPTY_ATOMS: &str = r#"{"schema_version":"2","atoms":[]}"#;

/// Write fixture `name` under `corpus_dir/atlas/` with ingest's writers.
fn write_fresh(name: &str, corpus_dir: &Path) {
    let atlas = corpus_dir.join(ATLAS_DIRNAME);
    std::fs::create_dir_all(&atlas).unwrap();
    match name {
        ATOM_STORE => {
            std::fs::write(atlas.join("atoms.json"), EMPTY_ATOMS).unwrap();
            let file = read_atlas_atoms(&atlas).unwrap();
            write_store_blocking(&atlas, name, file.atoms(), &[]).unwrap();
        }
        WIKI_STORE => {
            let meta = |links: Vec<(&str, &str)>| {
                serde_json::to_string(&WikipediaChunkMetadata {
                    section_name: "Lead".into(),
                    section_path: vec!["Lead".into()],
                    section_depth: 0,
                    section_type: "lead".into(),
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
                })
                .unwrap()
            };
            let ch = |id: u64, title: &str, m: String| StoredChunkWithMetadata {
                id,
                title: Some(title.into()),
                url: None,
                metadata_raw: Some(m),
            };
            let chunks = vec![
                ch(1, "Alpha", meta(vec![("Beta", "beta")])),
                ch(2, "Beta", meta(vec![])),
            ];
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(build_wikipedia_columnar_store_from_chunks(
                    &atlas, name, chunks,
                ))
                .unwrap();
        }
        other => panic!("no atlas store fixture named `{other}`"),
    }
}

/// What the leaf's walk opener reads from fixture `name` under `indexes`.
fn read_back(indexes: &Path, name: &str) -> String {
    let p = open_walk_provider_blocking(indexes, name).unwrap();
    let alpha_edges: Vec<String> = p
        .edges_from(WIKI_ALPHA_ATOM_ID)
        .iter()
        .map(|e| format!("{}->{} {:?}", e.source, e.target, e.edge_type))
        .collect();
    format!(
        "class={} corpus={} inventory={:?} alpha={} alpha_edges={alpha_edges:?}",
        p.provider_class(),
        p.atlas_corpus_id(),
        p.inventory(),
        p.atom(WIKI_ALPHA_ATOM_ID).is_some(),
    )
}

#[test]
fn checked_in_atlas_stores_read_like_freshly_written_ones() {
    assert_eq!(WIKI_ALPHA_ATOM_ID, wiki_atom_id("Alpha", WIKI_STORE));
    for name in [ATOM_STORE, WIKI_STORE] {
        let fresh = tempfile::tempdir().unwrap();
        write_fresh(name, &fresh.path().join(name));
        let checked_in = tempfile::tempdir().unwrap();
        copy_store_fixture(name, &checked_in.path().join(name)).unwrap();
        assert_eq!(
            read_back(fresh.path(), name),
            read_back(checked_in.path(), name),
            "fixture `{name}` no longer reads like a fresh store: run \
             regenerate_atlas_store_fixtures"
        );
    }
    // The wiki fixture is the walk svrn's test asserts: Alpha, one edge.
    let checked_in = tempfile::tempdir().unwrap();
    copy_store_fixture(WIKI_STORE, &checked_in.path().join(WIKI_STORE)).unwrap();
    let p = open_walk_provider_blocking(checked_in.path(), WIKI_STORE).unwrap();
    assert!(p.atom(WIKI_ALPHA_ATOM_ID).is_some());
    assert_eq!(p.edges_from(WIKI_ALPHA_ATOM_ID).len(), 1);
}

/// The engine half of svrn's atlas_step_reachability (sovereign-tools
/// tests/main), which walks the checked-in wiki store: every atom in it is an
/// Entity of ingest's wiki type under the id ingest mints for its title, Alpha
/// links to Beta, and the seed table svrn builds for it with the leaf's
/// `AnnSeedTable::build` holds what ingest's `build_persistent_ann_seed_table`
/// writes from the same entries.
#[tokio::test]
async fn the_wiki_fixture_holds_what_ingest_writes_for_the_walk() {
    use corpus_engine::enrichment::atlas::context::{
        build_persistent_ann_seed_table, AtlasContext, AtlasEntry,
    };
    use corpus_engine::enrichment::atlas::wiki_store::WIKI_ENTITY_TYPE;
    use corpus_engine_atlas_reader::ann_store::AnnSeedTable;
    use corpus_engine_atlas_reader::opener::open_walk_provider;
    use understanding_vocab::atoms::AtomType;

    let checked_in = tempfile::tempdir().unwrap();
    copy_store_fixture(WIKI_STORE, &checked_in.path().join(WIKI_STORE)).unwrap();
    let p = open_walk_provider(checked_in.path(), WIKI_STORE)
        .await
        .unwrap();
    let titles = ["Alpha", "Beta"];
    for title in titles {
        let id = wiki_atom_id(title, WIKI_STORE);
        let atom = p
            .atom(&id)
            .unwrap_or_else(|| panic!("the wiki fixture must hold `{title}` as {id}"));
        assert_eq!(atom.name(), title);
        assert_eq!(atom.kind(), AtomType::Entity);
        assert_eq!(atom.subtype(), WIKI_ENTITY_TYPE);
    }
    let alpha_edges = p.edges_from(WIKI_ALPHA_ATOM_ID);
    assert_eq!(alpha_edges.len(), 1);
    assert_eq!(alpha_edges[0].target, wiki_atom_id("Beta", WIKI_STORE));

    let rows: Vec<(String, Vec<f32>)> = titles
        .iter()
        .enumerate()
        .map(|(i, t)| (wiki_atom_id(t, WIKI_STORE), vec![1.0 + i as f32; 4]))
        .collect();
    let atlas = checked_in.path().join(WIKI_STORE).join(ATLAS_DIRNAME);
    let entries = titles
        .iter()
        .zip(&rows)
        .map(|(t, (id, v))| AtlasEntry {
            atom_id: id.clone(),
            canonical_name: t.to_string(),
            embed_text: t.to_string(),
            embedding: v.clone(),
        })
        .collect();
    build_persistent_ann_seed_table(
        &atlas,
        &AtlasContext {
            atlas_corpus_id: WIKI_STORE.to_string(),
            entries,
            top_k: 12,
        },
    )
    .await
    .unwrap();
    let mut by_ingest = AnnSeedTable::open_for_atlas(&atlas)
        .await
        .unwrap()
        .all_rows()
        .await
        .unwrap();
    let leaf_dir = tempfile::tempdir().unwrap();
    let mut by_leaf = AnnSeedTable::build(leaf_dir.path(), &rows)
        .await
        .unwrap()
        .all_rows()
        .await
        .unwrap();
    by_ingest.sort_by(|a, b| a.0.cmp(&b.0));
    by_leaf.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(by_ingest, by_leaf);
}

/// Rewrites the checked-in fixtures from ingest's writers.
#[test]
#[ignore = "rewrites shared/crates/corpus-engine-atlas-reader/testdata/stores"]
fn regenerate_atlas_store_fixtures() {
    for name in [ATOM_STORE, WIKI_STORE] {
        let dir = store_fixture_dir(name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        write_fresh(name, &dir);
    }
}
